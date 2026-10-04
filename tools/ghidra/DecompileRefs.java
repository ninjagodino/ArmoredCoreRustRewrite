// Decompiles every function that references one of the given addresses (hex) and writes
// them to private/ghidra/out/<address>.c. Run headless against the ACVD project:
//   analyzeHeadless private/ghidra ACVD -process EBOOT.elf -noanalysis -readOnly
//     -scriptPath tools/ghidra -postScript DecompileRefs.java 0x1ab1748 ...
import java.io.File;
import java.io.PrintWriter;
import java.util.LinkedHashSet;
import java.util.Set;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;

public class DecompileRefs extends GhidraScript {
    @Override
    protected void run() throws Exception {
        File out = new File(getProjectRootFolder().getProjectLocator().getProjectDir().getParentFile(), "out");
        out.mkdirs();
        DecompInterface d = new DecompInterface();
        d.openProgram(currentProgram);
        for (String arg : getScriptArgs()) {
            if (arg.startsWith("ret:")) {
                // "ret:<function>" (':' because the .bat launcher splits on '='): it returns, though analysis marked it non-returning; give its
                // call sites their fall-through back. List these before the functions they affect.
                Function r = getFunctionAt(toAddr(arg.substring(4)));
                if (r != null) {
                    r.setNoReturn(false);
                    for (Reference ref : getReferencesTo(r.getEntryPoint())) {
                        ghidra.program.model.listing.Instruction ins = getInstructionAt(ref.getFromAddress());
                        if (ins != null && ref.getReferenceType().isCall()) {
                            ins.clearFallThroughOverride();
                            if (ins.getFlowOverride() != ghidra.program.model.listing.FlowOverride.NONE) ins.setFlowOverride(ghidra.program.model.listing.FlowOverride.NONE);
                            Address next = ins.getMaxAddress().add(1);
                            if (getInstructionAt(next) == null) disassemble(next);                        }
                    }
                }
                println(arg + ": " + (r == null ? "no function" : "returns"));
                continue;
            }
            if (arg.startsWith("@")) {
                // "@<code address>": decompile the function containing it.
                // "@<start>-<end>": the function is exactly start..end (Xbox 360 .pdata bounds).
                String[] span = arg.substring(1).split("-");
                Address a = toAddr(span[0]);
                Function f = getFunctionContaining(a);
                if (span.length == 2) {
                    Address end = toAddr(span[1]).subtract(1);
                    ghidra.program.model.address.AddressSet body = new ghidra.program.model.address.AddressSet(a, end);
                    if (f != null && !f.getEntryPoint().equals(a)) f = null;
                    ghidra.app.cmd.disassemble.DisassembleCommand dis = new ghidra.app.cmd.disassemble.DisassembleCommand(body, body, true);
                    dis.applyTo(currentProgram, monitor);
                    if (f == null) {
                        java.util.Iterator<Function> old = currentProgram.getFunctionManager().getFunctionsOverlapping(body);
                        while (old.hasNext()) removeFunction(old.next());
                        f = currentProgram.getFunctionManager().createFunction(null, a, body, ghidra.program.model.symbol.SourceType.DEFAULT);
                    } else {
                        f.setBody(body);
                    }
                } else {
                    if (f == null && getInstructionAt(a) == null) disassemble(a);
                    if (f == null) f = createFunction(a, null);
                    if (f != null) ghidra.app.cmd.function.CreateFunctionCmd.fixupFunctionBody(currentProgram, f, monitor);
                }
                try (PrintWriter w = new PrintWriter(new File(out, span[0] + ".c"))) {
                    if (f == null) {
                        w.println("// no function at " + a);
                    } else {
                        setToc(f);
                        DecompileResults res = d.decompileFunction(f, 120, monitor);
                        w.println("// ---- " + f.getName() + " @ " + f.getEntryPoint());
                        w.println(res.decompileCompleted() ? res.getDecompiledFunction().getC() : "// failed: " + res.getErrorMessage());
                    }
                }
                println(arg + ": " + (f == null ? "no function" : f.getName() + " " + f.getBody().getMinAddress() + ".." + f.getBody().getMaxAddress()));
                continue;
            }
            Address target = toAddr(arg);
            Set<Function> fns = new LinkedHashSet<>();
            Set<Address> sites = new LinkedHashSet<>();
            sites.add(target);
            // PPC code reaches data through 32-bit pointers in the TOC: follow every slot
            // holding the address too.
            long v = target.getOffset();
            byte[] be = { (byte) (v >> 24), (byte) (v >> 16), (byte) (v >> 8), (byte) v };
            Address at = currentProgram.getMinAddress();
            while (at != null && (at = find(at, be)) != null) {
                sites.add(at);
                at = at.add(1);
            }
            // SNC also loads a nearby base from the TOC and adds the offset: take TOC slots
            // (r2 +- 0x8000) pointing up to 0x1000 bytes before the address.
            Address toc = toAddr(0x01D9CA50L);
            for (long o = -0x8000; o < 0x8000; o += 4) {
                Address slot = toc.add(o);
                try {
                    long p = getInt(slot) & 0xffffffffL;
                    if (p <= v && v - p < 0x1000 && p != v) {
                        sites.add(slot);
                        println(arg + ": TOC slot " + slot + " -> " + Long.toHexString(p) + " (+" + Long.toHexString(v - p) + ")");
                    }
                } catch (Exception e) {
                }
            }
            for (Address s : sites) {
                for (Reference r : getReferencesTo(s)) {
                    Function f = getFunctionContaining(r.getFromAddress());
                    if (f != null) fns.add(f);
                }
            }
            println(arg + ": pointer slots " + sites);
            try (PrintWriter w = new PrintWriter(new File(out, arg + ".c"))) {
                w.println("// references to " + arg + ": " + fns.size() + " functions");
                for (Function f : fns) {
                    setToc(f);
                    DecompileResults res = d.decompileFunction(f, 120, monitor);
                    w.println("// ---- " + f.getName() + " @ " + f.getEntryPoint());
                    w.println(res.decompileCompleted() ? res.getDecompiledFunction().getC() : "// failed: " + res.getErrorMessage());
                }
            }
            println(arg + ": " + fns.size() + " functions");
        }
        d.dispose();
    }

    // Each code range runs with its own TOC (the r2 its .opd descriptors carry); without it
    // the decompiler resolves r2-relative loads against the wrong table.
    private static final long[][] TOCS = {
        { 0x12a64L, 0x11e07cL, 0x1DCC8C0L }, { 0x11e07cL, 0x1a3e88L, 0x1DDC82CL },
        { 0x1a3e88L, 0x5b0c44L, 0x1D9CA50L }, { 0x5b0c44L, 0xa31b78L, 0x1DACA24L },
        { 0xa31b78L, 0xf28220L, 0x1DBCA04L }, { 0xf28220L, 0x16e4bc4L, 0x1DCC8C0L },
        { 0x16e4bc4L, 0x1a60000L, 0x1DDC82CL },
    };

    private void setToc(Function f) throws Exception {
        long e = f.getEntryPoint().getOffset();
        for (long[] r : TOCS) {
            if (e >= r[0] && e < r[1]) {
                ghidra.program.model.lang.Register r2 = currentProgram.getRegister("r2");
                currentProgram.getProgramContext().setValue(r2, f.getBody().getMinAddress(), f.getBody().getMaxAddress(), java.math.BigInteger.valueOf(r[2]));
                return;
            }
        }
    }
}
