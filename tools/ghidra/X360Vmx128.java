// Annotates Xenon VMX128 instructions that Ghidra's PowerPC module cannot decode.
//
// Ghidra 12's ppc_32_be language knows Altivec, but not the 360's VMX128 variant:
// the 128-register forms live in primary opcodes 4/5/6 and steal register bits
// from the Altivec XO field, which SLEIGH cannot express (token fields are a
// single contiguous bit range). Ghidra therefore stops at the first VMX128 word
// and, worse, often resynchronises 2 bytes off, inventing instructions in the
// rest of the function.
//
// This script decodes the words itself from tools/x360dis/vmx128.txt (generated
// from Xenia's ppc-instructions.xml by tools/x360dis/gen_vmx128_table.py) and:
//   * writes the correct text as an end-of-line comment on each VMX128 word,
//   * flags words where Ghidra holds a different (misdecoded) instruction,
//   * resolves lis/addi constants and creates read/write references for the
//     vector load/stores, so constant pools show up as xrefs,
//   * with --fix, replaces the misaligned/wrong instructions covering a VMX128
//     word with 4 raw bytes carrying the comment (restores 4-byte alignment;
//     the decompiler then shows undefined instead of nonsense).
//
// Usage (bridge or headless):
//   ghidra.exe script run tools/ghidra/X360Vmx128.java -- tools/x360dis/vmx128.txt
//   ghidra.exe script run tools/ghidra/X360Vmx128.java -- tools/x360dis/vmx128.txt 82cc6548 --fix
//   ghidra.exe script run tools/ghidra/X360Vmx128.java -- tools/x360dis/vmx128.txt 82cc6548 82bc2dd0
//
// Addresses are function entries or any address inside a .pdata function body;
// with no addresses, every .pdata function is scanned.

import java.io.BufferedReader;
import java.io.File;
import java.io.FileReader;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.data.Undefined4DataType;
import ghidra.program.model.lang.Register;
import ghidra.program.model.listing.CommentType;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.listing.Listing;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.symbol.RefType;
import ghidra.program.model.symbol.ReferenceManager;
import ghidra.program.model.symbol.SourceType;

public class X360Vmx128 extends GhidraScript {
    private static final long PDATA = 0x8225c200L, PDATA_SIZE = 0x99a38L;

    private static final class Row {
        final boolean vmx;
        final int primary;
        final int[][] spans;
        final int key;
        final String mnem;
        final String form;
        final String template;
        Row(boolean vmx, int primary, int[][] spans, int key, String mnem,
            String form, String template) {
            this.vmx = vmx;
            this.primary = primary;
            this.spans = spans;
            this.key = key;
            this.mnem = mnem;
            this.form = form;
            this.template = template;
        }
    }

    private static final class Group {
        final int primary;
        final int[][] spans;
        final Map<Integer, Row> byKey = new LinkedHashMap<>();
        Group(int primary, int[][] spans) {
            this.primary = primary;
            this.spans = spans;
        }
    }

    private final List<Row> rows = new ArrayList<>();
    private final Map<Integer, List<Group>> groups = new HashMap<>();
    private final Map<String, Long> known = new HashMap<>();
    private final Map<String, Boolean> fromLis = new HashMap<>();
    private final Map<String, Integer> hits = new HashMap<>();
    private Memory mem;
    private Listing listing;
    private ReferenceManager refs;
    private int misdecoded, undefinedWords, refsMade, scanned;
    private boolean dumpRefs;

    @Override
    protected void run() throws Exception {
        boolean fix = false;
        boolean all = false;
        boolean dumpRefs = false;
        String tablePath = null;
        List<String> targets = new ArrayList<>();
        for (String arg : getScriptArgs()) {
            if (arg.equals("--fix")) {
                fix = true;
            } else if (arg.equals("--all")) {
                all = true;
            } else if (arg.equals("--dump-refs")) {
                dumpRefs = true;
            } else if (arg.endsWith(".txt")) {
                tablePath = arg;
            } else if (!arg.startsWith("--")) {
                targets.add(arg);
            }
        }
        if (targets.isEmpty() && !all) {
            printerr("pass one or more function addresses, or --all (86k VMX128 "
                     + "words in this image; annotated comments are permanent)");
            return;
        }
        if (tablePath == null) {
            File scripts = new File(getSourceFile().getAbsolutePath()).getParentFile();
            for (String candidate : new String[] {
                    "tools/x360dis/vmx128.txt",
                    new File(scripts.getParentFile(), "x360dis/vmx128.txt").getPath() }) {
                if (new File(candidate).isFile()) {
                    tablePath = candidate;
                    break;
                }
            }
        }
        if (tablePath == null || !new File(tablePath).isFile()) {
            printerr("vmx128.txt not found; pass it as the first script argument "
                     + "(tools/x360dis/vmx128.txt)");
            return;
        }
        this.dumpRefs = dumpRefs;
        loadTable(tablePath);

        mem = currentProgram.getMemory();
        listing = currentProgram.getListing();
        refs = currentProgram.getReferenceManager();
        List<long[]> bodies = targets.isEmpty() ? pdataBodies() : bodiesFor(targets);
        println("X360Vmx128: " + bodies.size() + " function(s), fix=" + fix);

        for (long[] body : bodies) {
            if (monitor.isCancelled()) break;
            scanBody(body[0], body[1], fix);
        }

        println("--- summary ---");
        println("words scanned: " + scanned + ", VMX128: " + totalHits()
                + ", misdecoded: " + misdecoded + ", undefined: " + undefinedWords
                + ", refs created: " + refsMade
                + (fix ? ", fixed: " + fixedCount : ""));
        List<Map.Entry<String, Integer>> top = new ArrayList<>(hits.entrySet());
        top.sort((a, b) -> b.getValue() - a.getValue());
        for (int i = 0; i < Math.min(20, top.size()); i++) {
            println(String.format("  %-16s %6d", top.get(i).getKey(), top.get(i).getValue()));
        }
    }

    private int fixedCount;
    private int totalHits() {
        int n = 0;
        for (int v : hits.values()) n += v;
        return n;
    }

    private void loadTable(String path) throws Exception {
        try (BufferedReader r = new BufferedReader(new FileReader(path))) {
            String line;
            while ((line = r.readLine()) != null) {
                line = line.trim();
                if (line.isEmpty() || line.startsWith("#")) continue;
                String[] f = line.split("\\|", 7);
                if (f.length != 7) continue;
                int[][] spans = parseSpans(f[2]);
                Row row = new Row(f[0].equals("v"), Integer.parseInt(f[1]), spans,
                        Integer.parseInt(f[3].substring(2), 2), f[4], f[5], f[6]);
                rows.add(row);
                List<Group> list = groups.get(row.primary);
                if (list == null) {
                    list = new ArrayList<>();
                    groups.put(row.primary, list);
                }
                if (list.isEmpty() || !sameSpans(list.get(list.size() - 1).spans, spans)) {
                    list.add(new Group(row.primary, spans));
                }
                list.get(list.size() - 1).byKey.put(row.key, row);
            }
        }
    }

    private static int[][] parseSpans(String text) {
        String[] parts = text.split(",");
        int[][] spans = new int[parts.length][];
        for (int i = 0; i < parts.length; i++) {
            String[] ab = parts[i].split("-");
            spans[i] = new int[] { Integer.parseInt(ab[0]), Integer.parseInt(ab[1]) };
        }
        return spans;
    }

    private static boolean sameSpans(int[][] a, int[][] b) {
        if (a.length != b.length) return false;
        for (int i = 0; i < a.length; i++) {
            if (a[i][0] != b[i][0] || a[i][1] != b[i][1]) return false;
        }
        return true;
    }

    private static int extract(int word, int left, int right) {
        return (word >>> (31 - right)) & ((1 << (right - left + 1)) - 1);
    }

    private static int key(int word, int[][] spans) {
        int top = 0;
        for (int[] s : spans) top = Math.max(top, s[1]);
        int value = 0;
        for (int[] s : spans) {
            value |= extract(word, s[0], s[1]) << (top - s[1]);
        }
        return value;
    }

    private Row decode(int word) {
        List<Group> list = groups.get(word >>> 26);
        if (list == null) return null;
        for (Group g : list) {
            Row row = g.byKey.get(key(word, g.spans));
            if (row != null) return row;
        }
        return null;
    }

    private static int bits(int word, int lo, int hi) {
        return (word >>> lo) & ((1 << (hi - lo + 1)) - 1);
    }

    private static Map<String, Integer> fields(int word, String form) {
        Map<String, Integer> f = new HashMap<>();
        int vd = bits(word, 21, 25) | (bits(word, 2, 3) << 5);
        int va = bits(word, 16, 20) | (bits(word, 5, 5) << 5) | (bits(word, 10, 10) << 6);
        int vb = bits(word, 11, 15) | (bits(word, 0, 1) << 5);
        switch (form) {
        case "VX128":
        case "VX128_2":
            f.put("VD", vd); f.put("VA", va); f.put("VB", vb); f.put("VC", bits(word, 6, 8));
            break;
        case "VX128_1":
            f.put("VD", vd); f.put("VS", vd); f.put("RA0", bits(word, 16, 20));
            f.put("RB", bits(word, 11, 15));
            break;
        case "VX128_3": {
            int uimm = bits(word, 16, 20);
            f.put("VD", vd); f.put("VB", vb); f.put("UIMM", uimm); f.put("IMM", uimm);
            f.put("SIMM", ((uimm << 27) >> 27));
            break;
        }
        case "VX128_4":
            f.put("VD", vd); f.put("VB", vb); f.put("IMM", bits(word, 16, 20));
            f.put("z", bits(word, 6, 7));
            break;
        case "VX128_5":
            f.put("VD", vd); f.put("VA", va); f.put("VB", vb); f.put("SHB", bits(word, 6, 9));
            break;
        case "VX128_R":
            f.put("VD", vd); f.put("VA", va); f.put("VB", vb); f.put("Rc", bits(word, 6, 6));
            break;
        case "VX128_P":
            f.put("VD", vd); f.put("VB", vb);
            f.put("UIMM", bits(word, 16, 20) | (bits(word, 6, 8) << 5));
            break;
        default:
            break;
        }
        return f;
    }

    private static String operand(String name, int value) {
        switch (name) {
        case "VD": case "VS": case "VA": case "VB": case "VC":
            return "vr" + value;
        case "RA0": case "RB":
            return "r" + value;
        case "UIMM":
            return String.format("0x%X", value);
        default:
            return Integer.toString(value);
        }
    }

    private static String text(Row row, int word) {
        Map<String, Integer> values = fields(word, row.form);
        StringBuilder sb = new StringBuilder();
        StringBuilder token = null;
        for (char c : row.template.toCharArray()) {
            if (c == '[') {
                token = new StringBuilder();
            } else if (c == ']' && token != null) {
                String name = token.toString();
                if (name.equals("Rc")) {
                    sb.append(values.getOrDefault("Rc", 0) != 0 ? "." : "");
                } else {
                    sb.append(operand(name, values.get(name)));
                }
                token = null;
            } else if (token != null) {
                token.append(c);
            } else {
                sb.append(c);
            }
        }
        return sb.toString();
    }

    private List<long[]> pdataBodies() {
        List<long[]> out = new ArrayList<>();
        for (long o = 0; o < PDATA_SIZE; o += 8) {
            try {
                long begin = getInt(toAddr(PDATA + o)) & 0xffffffffL;
                long words = (getInt(toAddr(PDATA + o + 4)) >>> 8) & 0x3fffff;
                if (begin != 0 && words != 0) {
                    out.add(new long[] { begin, begin + words * 4 });
                }
            } catch (Exception e) {
                break;
            }
        }
        return out;
    }

    private List<long[]> bodiesFor(List<String> targets) {
        List<long[]> all = pdataBodies();
        List<long[]> out = new ArrayList<>();
        for (String t : targets) {
            long addr = Long.parseLong(t.replace("0x", ""), 16);
            for (long[] body : all) {
                if (addr >= body[0] && addr < body[1]) {
                    out.add(body);
                    break;
                }
            }
        }
        return out;
    }

    private void scanBody(long lo, long hi, boolean fix) {
        known.clear();
        fromLis.clear();
        for (long a = lo; a < hi; a += 4) {
            Address addr = toAddr(a);
            int word;
            try {
                word = mem.getInt(addr);
            } catch (Exception e) {
                break;
            }
            Row row = decode(word);
            scanned++;
            if (row == null || !row.vmx) {
                Instruction insn = listing.getInstructionAt(addr);
                if (insn == null || insn.getLength() != 4) {
                    // Desynchronised stretch: trust nothing until alignment returns.
                    known.clear();
                    fromLis.clear();
                } else {
                    track(insn, a);
                }
                continue;
            }
            hits.merge(row.mnem, 1, Integer::sum);
            String correct = text(row, word);
            Instruction insn = listing.getInstructionContaining(addr);
            boolean wrong = insn == null || insn.getAddress().getOffset() != a
                    || !insn.getMnemonicString().equals(row.mnem);
            String note = "x360dis: " + correct;
            if (wrong) {
                if (insn == null) undefinedWords++; else misdecoded++;
                note += "  [ghidra: " + (insn == null ? "undefined"
                        : insn.toString() + " @ " + insn.getAddress()) + "]";
                if (fix) {
                    for (Instruction cover : listing.getInstructions(
                            new ghidra.program.model.address.AddressSet(addr, addr.add(3)), true)) {
                        listing.clearCodeUnits(cover.getMinAddress(), cover.getMaxAddress(), false);
                    }
                    try {
                        createData(addr, new Undefined4DataType());
                    } catch (Exception e) {
                        // already defined, or overlapping data: comment still applies
                    }
                    fixedCount++;
                }
            }
            // Comments need a code unit at their address. With --fix the 4 bytes
            // were just created; without it, hang the note off whatever bogus
            // instruction currently covers the word (that is the symptom we flag).
            Address noteAt = addr;
            if (listing.getCodeUnitAt(addr) == null) {
                if (insn == null) {
                    noteAt = null;
                } else {
                    noteAt = insn.getAddress();
                    note = String.format("@+0x%x %s", a - insn.getAddress().getOffset(), note);
                }
            }
            if (noteAt != null) {
                listing.setComment(noteAt, CommentType.EOL, note);
            }

            if (row.mnem.startsWith("lv") || row.mnem.startsWith("st")) {
                Map<String, Integer> f = fields(word, row.form);
                int ra = f.getOrDefault("RA0", 0), rb = f.getOrDefault("RB", 0);
                Long base = ra == 0 ? Long.valueOf(0) : known.get("r" + ra);
                Long index = known.get("r" + rb);
                if (base != null && index != null) {
                    long ea = (base + index) & 0xffffffffL;
                    try {
                        Address target = toAddr(ea);
                        if (mem.contains(target)) {
                            RefType type = row.mnem.startsWith("st") ? RefType.WRITE : RefType.READ;
                            refs.addMemoryReference(noteAt != null ? noteAt : addr, target,
                                    type, SourceType.USER_DEFINED, 0);
                            refsMade++;
                            if (dumpRefs) {
                                println(String.format("ref %08x %-14s ra=r%d rb=r%d base=%x idx=%x -> %08x", a, row.mnem, ra, rb, base, index, ea));
                            }
                            if (noteAt != null && row.mnem.startsWith("lv")
                                    && mem.contains(target.add(15))) {
                                int w0 = mem.getInt(target), w1 = mem.getInt(target.add(4));
                                int w2 = mem.getInt(target.add(8)), w3 = mem.getInt(target.add(12));
                                String comment = listing.getComment(CommentType.EOL, noteAt);
                                listing.setComment(noteAt, CommentType.EOL,
                                        (comment != null ? comment : note)
                                        + String.format("  = %08x [%g %g %g %g]", ea,
                                                Float.intBitsToFloat(w0), Float.intBitsToFloat(w1),
                                                Float.intBitsToFloat(w2), Float.intBitsToFloat(w3)));
                            }
                        }
                    } catch (Exception e) {
                        // unresolved target: leave the plain comment
                    }
                }
            }
        }
    }

    private static final Pattern MNEM =
            Pattern.compile("^(\\w+)\\s+(.*)$");

    private void track(Instruction insn, long at) {
        // Ghidra resynchronises 2 bytes off after a VMX128 word, so only trust
        // instructions that start exactly on our 4-byte walk position.
        if (insn.getAddress().getOffset() != at) {
            known.clear();
            fromLis.clear();
            return;
        }
        String mnem = insn.getMnemonicString();
        Matcher m = MNEM.matcher(insn.toString().trim());
        if (!m.matches()) return;
        String[] ops = m.group(2).replace(" ", "").split(",");
        // A call clobbers the volatile half of the register file.
        if (mnem.equals("bl") || mnem.equals("bctrl") || mnem.equals("bcl")) {
            for (int r = 0; r <= 12; r++) {
                known.remove("r" + r);
                fromLis.remove("r" + r);
            }
            return;
        }
        // Invalidate the destination before the constant-forming forms
        // re-establish it (Ghidra's getResultObjects() is empty for ALU ops).
        if (ops.length > 0 && !mnem.startsWith("st") && ops[0].matches("r\\d+")) {
            known.remove(ops[0]);
            fromLis.remove(ops[0]);
        }
        try {
            if ((mnem.equals("lis") || mnem.equals("addis")) && ops.length == 2) {
                known.put(ops[0], ((long) imm16(ops[1]) & 0xffff) << 16);
                fromLis.put(ops[0], Boolean.TRUE);
            } else if ((mnem.equals("addi") || mnem.equals("li")) && ops.length == 2) {
                known.put(ops[0], imm16(ops[1]) & 0xffffffffL);
            } else if (mnem.equals("ori")) {
                Long src = known.get(ops[1]);
                if (src != null) {
                    known.put(ops[0], src | (imm16(ops[2]) & 0xffffL));
                    fromLis.put(ops[0], fromLis.get(ops[1]));
                }
            } else if (mnem.equals("addi") && ops.length == 3) {
                Long src = known.get(ops[1]);
                if (src != null) {
                    known.put(ops[0], (src + imm16(ops[2])) & 0xffffffffL);
                    fromLis.put(ops[0], fromLis.get(ops[1]));
                }
            } else if (mnem.equals("mr") && ops.length == 2) {
                Long src = known.get(ops[1]);
                if (src != null) {
                    known.put(ops[0], src);
                    fromLis.put(ops[0], fromLis.get(ops[1]));
                }
            }
        } catch (RuntimeException e) {
            // not a constant-forming form
        }
    }

    private static long imm16(String text) {
        String s = text.trim();
        if (s.startsWith("-0x")) return -Long.parseLong(s.substring(3), 16);
        if (s.startsWith("0x")) return Long.parseLong(s.substring(2), 16);
        return Long.parseLong(s);
    }
}

