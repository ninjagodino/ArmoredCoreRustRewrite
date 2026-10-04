// Makes the functions of the Xbox 360 image match its .pdata table (begin address, length in
// words at bits 8..29 of the second word): disassembles each body, drops functions whose bounds
// disagree or that overlap a .pdata body, and creates the missing ones. Safe to re-run. Writable,
// against the 360 project ghidra-cli uses:
//   analyzeHeadless private/ghidra360cli ACV2 -process ACV2.pe -noanalysis
//     -scriptPath tools/ghidra -postScript X360Pdata.java
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;

import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressRange;
import ghidra.program.model.address.AddressSet;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.listing.Listing;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;
import ghidra.program.model.symbol.SourceType;

public class X360Pdata extends GhidraScript {
    private static final long PDATA = 0x8225c200L, PDATA_SIZE = 0x99a38L;

    @Override
    protected void run() throws Exception {
        FunctionManager fm = currentProgram.getFunctionManager();
        TreeMap<Address, AddressSet> bodies = new TreeMap<>();
        AddressSet code = new AddressSet();
        for (long o = 0; o < PDATA_SIZE; o += 8) {
            long begin = getInt(toAddr(PDATA + o)) & 0xffffffffL;
            long words = (getInt(toAddr(PDATA + o + 4)) >>> 8) & 0x3fffff;
            if (begin == 0 || words == 0) continue;
            AddressSet body = new AddressSet(toAddr(begin), toAddr(begin + words * 4 - 1));
            bodies.put(toAddr(begin), body);
            code.add(body);
        }
        monitor.setMessage("disassembling " + code.getNumAddresses() + " bytes");
        new DisassembleCommand(code, code, false).applyTo(currentProgram, monitor);
        // Calls into the middle of the register save/restore helpers (0x8326e3a0..) leave the
        // rest of the caller undefined, so restart at every gap until none closes.
        Listing listing = currentProgram.getListing();
        for (int pass = 0; ; pass++) {
            AddressSet gaps = new AddressSet(code);
            for (Instruction i : listing.getInstructions(code, true)) {
                if (i.getFlowType().isCall() && i.getFallThrough() == null) i.clearFallThroughOverride();
                gaps.delete(i.getMinAddress(), i.getMaxAddress());
            }
            AddressSet starts = new AddressSet();
            for (AddressRange r : gaps) starts.add(r.getMinAddress());
            println("pass " + pass + ": " + gaps.getNumAddresses() + " undefined bytes in " + gaps.getNumAddressRanges() + " gaps");
            if (starts.isEmpty() || pass == 8) break;
            long before = gaps.getNumAddresses();
            new DisassembleCommand(starts, code, true).applyTo(currentProgram, monitor);
            long left = 0;
            for (AddressRange r : gaps) for (Address a = r.getMinAddress(); a != null && a.compareTo(r.getMaxAddress()) <= 0; a = a.add(4)) if (listing.getInstructionContaining(a) == null) left += 4;
            if (left == before) break;
        }

        List<Function> drop = new ArrayList<>();
        for (Function f : fm.getFunctions(true)) {
            AddressSet want = bodies.get(f.getEntryPoint());
            if (want != null ? !want.hasSameAddresses(f.getBody()) : f.getBody().intersects(code)) drop.add(f);
        }
        for (Function f : drop) fm.removeFunction(f.getEntryPoint());

        int made = 0, failed = 0;
        for (var e : bodies.entrySet()) {
            monitor.checkCancelled();
            if (fm.getFunctionAt(e.getKey()) != null) continue;
            try {
                fm.createFunction(null, e.getKey(), e.getValue(), SourceType.DEFAULT);
                made++;
            } catch (Exception x) {
                failed++;
                println(e.getKey() + ": " + x.getMessage());
            }
        }
        println(".pdata " + bodies.size() + ": dropped " + drop.size() + ", created " + made + ", failed " + failed + ", functions now " + fm.getFunctionCount());
    }
}
