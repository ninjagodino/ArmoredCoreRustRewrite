// Undoes call flow damage on the Xbox 360 image: `bl` calls that analysis turned into call-return
// (flow override) or left without a fall-through, and every non-returning mark (most were wrong; a
// right one only costs a little extra flow inside the .pdata bounds). The register
// save/restore helpers (0x8326e3a0..) are entered mid-body by `bl`, which is what trips it.
// Prints counts; "-- dry" only reports. Run through ghidra-cli:
//   ghidra script run tools/ghidra/X360CallFlow.java [-- dry]
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.FlowOverride;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;

public class X360CallFlow extends GhidraScript {
    @Override
    protected void run() throws Exception {
        boolean dry = getScriptArgs().length > 0 && getScriptArgs()[0].equals("dry");
        int noret = 0, over = 0, fall = 0;
        for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
            if (f.hasNoReturn()) { noret++; if (!dry) f.setNoReturn(false); }
        }
        for (Instruction i : currentProgram.getListing().getInstructions(true)) {
            if (!i.getMnemonicString().equals("bl")) continue;
            if (i.getFlowOverride() != FlowOverride.NONE) { over++; if (!dry) i.setFlowOverride(FlowOverride.NONE); }
            if (i.isFallThroughOverridden()) { fall++; if (!dry) i.clearFallThroughOverride(); }
        }
        println("non-returning " + noret + ", bl flow overrides " + over + ", bl fall-through overrides " + fall + (dry ? " (dry)" : " (cleared)"));
    }
}
