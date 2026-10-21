// Exports an analyzed program as JSON lines, for the toolkit's knowledge files (DESIGN.md §5).
// Run through Ghidra's headless analyzer:
//   analyzeHeadless ... -postScript RecExport.java <out.jsonl> [decompile] [functions <file>]
// With a functions file (a linear address a line: entries found some other way, such as interrupt
// handlers only a vector reaches), functions are created there first and the changes analyzed.
// One JSON object per line: a "program" record, then "block", "function", "call" and "ref" records
// (an instruction's reference to data: "read", "write" or "data" for an address taken), then one
// "code" record per function with its disassembly (and its decompilation when asked).
// Function records carry Ghidra's Function ID hashes ("fid"), which library signature packs
// (`sigs/`) match.
// Addresses are linear (segment * 16 + offset) as Ghidra stores them; the toolkit turns them into
// load-image offsets with the program record's entry point.
//@category Rrecreate

import java.io.BufferedWriter;
import java.io.FileWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.feature.fid.hash.FidHashQuad;
import ghidra.feature.fid.service.FidService;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressRange;
import ghidra.program.model.address.SegmentedAddress;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.listing.InstructionIterator;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.Symbol;

public class RecExport extends GhidraScript {

	private BufferedWriter out;

	@Override
	public void run() throws Exception {
		String[] args = getScriptArgs();
		if (args.length < 1) {
			throw new IllegalArgumentException("usage: RecExport.java <out.jsonl> [decompile] [functions <file>]");
		}
		boolean decompile = false;
		for (int i = 1; i < args.length; i++) {
			if (args[i].equals("decompile")) {
				decompile = true;
			}
			else if (args[i].equals("functions") && i + 1 < args.length) {
				createFunctions(Path.of(args[++i]));
			}
		}
		out = new BufferedWriter(new FileWriter(args[0]));
		try {
			export(decompile);
		}
		finally {
			out.close();
		}
	}

	private void export(boolean decompile) throws Exception {
		Address entry = null;
		for (Symbol symbol : currentProgram.getSymbolTable().getSymbols("entry")) {
			entry = symbol.getAddress();
		}
		line("{\"record\":\"program\",\"name\":" + str(currentProgram.getName()) + ",\"language\":" +
			str(currentProgram.getLanguageID().getIdAsString()) + ",\"compiler\":" +
			str(currentProgram.getCompilerSpec().getCompilerSpecID().getIdAsString()) + ",\"entry\":" +
			address(entry) + ",\"ghidra\":" + str(getGhidraVersion()) + "}");
		for (MemoryBlock block : currentProgram.getMemory().getBlocks()) {
			line("{\"record\":\"block\",\"name\":" + str(block.getName()) + ",\"start\":" +
				address(block.getStart()) + ",\"size\":" + block.getSize() + ",\"execute\":" +
				block.isExecute() + ",\"initialized\":" + block.isInitialized() + "}");
		}
		List<Function> functions = new ArrayList<>();
		FidService fid = new FidService();
		for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
			if (monitor.isCancelled()) {
				return;
			}
			functions.add(f);
			StringBuilder ranges = new StringBuilder();
			for (AddressRange range : f.getBody()) {
				ranges.append(ranges.length() == 0 ? "" : ",").append("[").append(
					address(range.getMinAddress())).append(",").append(range.getLength()).append("]");
			}
			line("{\"record\":\"function\",\"address\":" + address(f.getEntryPoint()) + ",\"name\":" +
				str(f.getName()) + ",\"source\":" + str(f.getSymbol().getSource().toString()) +
				",\"convention\":" + str(f.getCallingConventionName()) + ",\"signature\":" +
				str(f.getSignature().getPrototypeString()) + ",\"thunk\":" + f.isThunk() +
				",\"fid\":" + fidHash(fid, f) + ",\"body\":[" + ranges + "]}");
		}
		for (Function f : functions) {
			InstructionIterator instructions = currentProgram.getListing().getInstructions(f.getBody(), true);
			for (Instruction insn : instructions) {
				for (Reference ref : insn.getReferencesFrom()) {
					if (ref.getReferenceType().isFlow() || !ref.getToAddress().isMemoryAddress()) {
						continue;
					}
					line("{\"record\":\"ref\",\"from\":" + address(insn.getAddress()) + ",\"to\":" +
						address(ref.getToAddress()) + ",\"kind\":\"" + refKind(ref) + "\"}");
				}
				if (!insn.getFlowType().isCall()) {
					continue;
				}
				for (Reference ref : insn.getReferencesFrom()) {
					if (!ref.getReferenceType().isCall()) {
						continue;
					}
					Function callee = getFunctionAt(ref.getToAddress());
					line("{\"record\":\"call\",\"caller\":" + address(f.getEntryPoint()) + ",\"site\":" +
						address(insn.getAddress()) + ",\"callee\":" +
						address(callee != null ? callee.getEntryPoint() : ref.getToAddress()) + "}");
				}
			}
		}
		DecompInterface decompiler = null;
		if (decompile) {
			decompiler = new DecompInterface();
			decompiler.openProgram(currentProgram);
		}
		for (Function f : functions) {
			if (monitor.isCancelled()) {
				break;
			}
			StringBuilder asm = new StringBuilder();
			for (Instruction insn : currentProgram.getListing().getInstructions(f.getBody(), true)) {
				asm.append(insn.getAddress()).append("  ").append(insn).append("\n");
			}
			String c = null;
			if (decompiler != null) {
				DecompileResults results = decompiler.decompileFunction(f, 60, monitor);
				if (results.decompileCompleted()) {
					c = results.getDecompiledFunction().getC();
				}
			}
			line("{\"record\":\"code\",\"address\":" + address(f.getEntryPoint()) + ",\"asm\":" + str(asm.toString()) +
				",\"c\":" + (c == null ? "null" : str(c)) + "}");
		}
		if (decompiler != null) {
			decompiler.dispose();
		}
	}

	/** Creates a function at each entry the file lists where Ghidra has none, then analyzes them. */
	private void createFunctions(Path file) throws Exception {
		int created = 0;
		for (String line : Files.readAllLines(file)) {
			if (line.isBlank()) {
				continue;
			}
			Address address = toAddr(Long.parseLong(line.trim()));
			if (getFunctionAt(address) != null) {
				continue;
			}
			if (getInstructionAt(address) == null) {
				disassemble(address);
			}
			if (createFunction(address, null) != null) {
				created++;
			}
			else {
				println("RecExport: no function could be made at " + address);
			}
		}
		analyzeChanges(currentProgram);
		println("RecExport: created " + created + " functions");
	}

	/** The kind of a data reference: "write", "read", or "data" (an address taken). */
	private static String refKind(Reference ref) {
		if (ref.getReferenceType().isWrite()) {
			return "write";
		}
		return ref.getReferenceType().isRead() ? "read" : "data";
	}

	/** Function ID hashes: code units, full hash, specific hash and its extra units; or null. */
	private static String fidHash(FidService fid, Function f) {
		try {
			FidHashQuad q = fid.hashFunction(f);
			if (q == null) {
				return "null";
			}
			return String.format("{\"units\":%d,\"full\":\"%016x\",\"specific\":\"%016x\",\"extra\":%d}",
				q.getCodeUnitSize(), q.getFullHash(), q.getSpecificHash(), q.getSpecificHashAdditionalSize());
		}
		catch (Exception e) {
			return "null";
		}
	}

	private void line(String json) throws Exception {
		out.write(json);
		out.write("\n");
	}

	/** A linear address with its segment:offset form, or null. */
	private static String address(Address a) {
		if (a == null) {
			return "null";
		}
		String text = a instanceof SegmentedAddress s
				? String.format("%04X:%04X", s.getSegment(), s.getSegmentOffset())
				: a.toString();
		return "{\"linear\":" + a.getOffset() + ",\"text\":" + str(text) + "}";
	}

	private static String str(String s) {
		if (s == null) {
			return "null";
		}
		StringBuilder b = new StringBuilder("\"");
		for (char c : s.toCharArray()) {
			switch (c) {
				case '"' -> b.append("\\\"");
				case '\\' -> b.append("\\\\");
				case '\n' -> b.append("\\n");
				case '\r' -> b.append("\\r");
				case '\t' -> b.append("\\t");
				default -> {
					if (c < 0x20) {
						b.append(String.format("\\u%04x", (int) c));
					}
					else {
						b.append(c);
					}
				}
			}
		}
		return b.append("\"").toString();
	}
}
