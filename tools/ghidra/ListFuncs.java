// Lists functions whose entry lies in [lo, hi) with body size and called functions, to
// private/ghidra/out/funcs_<lo>.txt. Args: lo hi (hex).
import java.io.File;
import java.io.PrintWriter;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ListFuncs extends GhidraScript {
    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        Address lo = toAddr(args[0]);
        Address hi = toAddr(args[1]);
        File out = new File(getProjectRootFolder().getProjectLocator().getProjectDir().getParentFile(), "out");
        out.mkdirs();
        try (PrintWriter w = new PrintWriter(new File(out, "funcs_" + args[0] + ".txt"))) {
            for (Function f : currentProgram.getFunctionManager().getFunctions(lo, true)) {
                if (f.getEntryPoint().compareTo(hi) >= 0) break;
                StringBuilder calls = new StringBuilder();
                for (Function c : f.getCalledFunctions(monitor)) {
                    calls.append(' ').append(c.getEntryPoint());
                }
                w.println(f.getEntryPoint() + " " + f.getBody().getNumAddresses() + " " + f.getName() + " ->" + calls);
            }
        }
    }
}
