// Writes the disassembly of the function containing each address (hex) to
// private/ghidra/out/<address>.asm. Run headless like DecompileRefs.java.
import java.io.File;
import java.io.PrintWriter;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;

public class DumpAsm extends GhidraScript {
    @Override
    protected void run() throws Exception {
        File out = new File(getProjectRootFolder().getProjectLocator().getProjectDir().getParentFile(), "out");
        out.mkdirs();
        for (String arg : getScriptArgs()) {
            Address a = toAddr(arg);
            Function f = getFunctionContaining(a);
            try (PrintWriter w = new PrintWriter(new File(out, arg + ".asm"))) {
                if (f == null) {
                    w.println("// no function at " + a);
                    continue;
                }
                w.println("// " + f.getName() + " @ " + f.getEntryPoint());
                for (Instruction i : currentProgram.getListing().getInstructions(f.getBody(), true)) {
                    w.println(i.getAddress() + "  " + i);
                }
            }
        }
    }
}
