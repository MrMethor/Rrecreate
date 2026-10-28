// Exports an analyzed program as JSON lines, for the toolkit's knowledge files (DESIGN.md §5).
// Run through Ghidra's headless analyzer:
//   analyzeHeadless ... -postScript RecExport.java <out.jsonl> [decompile] [functions <file>]
//       [signatures <file>] [only]
// With a functions file (a linear address a line: entries found some other way, such as interrupt
// handlers only a vector reaches), functions are created there first where there are none and the
// changes analyzed; with "only", the export has just those functions' records (to add to an export of
// the program made before, from the same analysis: no program or block records).
// A signatures file gives functions their register contracts, as a run showed them (D81): a line is
// "<linear address> <inputs> <outputs>", inputs register names joined by commas and outputs each a
// register, "DX:AX" or a flag ("CF", "ZF") joined by "+" (each "-" for none). The inputs become register
// parameters (beside the stack ones Ghidra found) and the outputs the return, so callers' code uses
// them: one is the return value; several are a structure of them (D88), stored in those registers.
// One JSON object per line: a "program" record, then "block", "function", "call" and "ref" records
// (an instruction's reference to data: "read", "write" or "data" for an address taken), then one
// "code" record per function with its disassembly (and its decompilation when asked).
// Function records carry Ghidra's Function ID hashes ("fid"), which library signature packs
// (`sigs/`) match.
// In real mode, a near call through memory (a table of handlers) stays in its code segment: its targets
// are the words it reads (Ghidra's read references from the call) as offsets in the caller's segment,
// made functions there, where Ghidra's analysis put them in another segment (D89).

// Addresses are linear (segment * 16 + offset) as Ghidra stores them; the toolkit turns them into
// load-image offsets with the program record's entry point.
//@category Rrecreate

import java.io.BufferedWriter;
import java.io.FileWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.feature.fid.hash.FidHashQuad;
import ghidra.feature.fid.service.FidService;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressRange;
import ghidra.program.model.address.SegmentedAddress;
import ghidra.program.model.address.SegmentedAddressSpace;
import ghidra.program.model.data.BooleanDataType;
import ghidra.program.model.data.ByteDataType;
import ghidra.program.model.data.CategoryPath;
import ghidra.program.model.data.DWordDataType;
import ghidra.program.model.data.DataType;
import ghidra.program.model.data.DataTypeConflictHandler;
import ghidra.program.model.data.StructureDataType;
import ghidra.program.model.data.WordDataType;
import ghidra.program.model.lang.Register;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Function.FunctionUpdateType;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.listing.InstructionIterator;
import ghidra.program.model.listing.Parameter;
import ghidra.program.model.listing.ParameterImpl;
import ghidra.program.model.listing.ReturnParameterImpl;
import ghidra.program.model.listing.Variable;
import ghidra.program.model.listing.VariableStorage;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.RefType;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.ReferenceManager;
import ghidra.program.model.symbol.SourceType;
import ghidra.program.model.symbol.Symbol;

public class RecExport extends GhidraScript {

	private BufferedWriter out;

	/** The functions the functions file lists (created or there before). */
	private final Set<Address> listed = new HashSet<>();

	@Override
	public void run() throws Exception {
		String[] args = getScriptArgs();
		if (args.length < 1) {
			throw new IllegalArgumentException("usage: RecExport.java <out.jsonl> [decompile] [functions <file>]");
		}
		boolean decompile = false;
		boolean only = false;
		Path functions = null;
		Path signatures = null;
		for (int i = 1; i < args.length; i++) {
			if (args[i].equals("decompile")) {
				decompile = true;
			}
			else if (args[i].equals("only")) {
				only = true;
			}
			else if (args[i].equals("functions") && i + 1 < args.length) {
				functions = Path.of(args[++i]);
			}
			else if (args[i].equals("signatures") && i + 1 < args.length) {
				signatures = Path.of(args[++i]);
			}
		}
		if (functions != null) {
			createFunctions(functions);
		}
		if (signatures != null) {
			applySignatures(signatures);
		}
		// After the functions added are analysed: their calls are among those to fix.
		fixNearCalls();
		out = new BufferedWriter(new FileWriter(args[0]));
		try {
			export(decompile, only);
		}
		finally {
			out.close();
		}
	}

	private void export(boolean decompile, boolean only) throws Exception {
		Address entry = null;
		for (Symbol symbol : currentProgram.getSymbolTable().getSymbols("entry")) {
			entry = symbol.getAddress();
		}
		if (!only) {
			programRecords(entry);
		}
		exportFunctions(decompile, only);
	}

	private void programRecords(Address entry) throws Exception {
		line("{\"record\":\"program\",\"name\":" + str(currentProgram.getName()) + ",\"language\":" +
			str(currentProgram.getLanguageID().getIdAsString()) + ",\"compiler\":" +
			str(currentProgram.getCompilerSpec().getCompilerSpecID().getIdAsString()) + ",\"entry\":" +
			address(entry) + ",\"ghidra\":" + str(getGhidraVersion()) + "}");
		for (MemoryBlock block : currentProgram.getMemory().getBlocks()) {
			line("{\"record\":\"block\",\"name\":" + str(block.getName()) + ",\"start\":" +
				address(block.getStart()) + ",\"size\":" + block.getSize() + ",\"execute\":" +
				block.isExecute() + ",\"initialized\":" + block.isInitialized() + "}");
		}
	}

	/** The functions' records (with "only", the listed functions'). */
	private void exportFunctions(boolean decompile, boolean only) throws Exception {
		List<Function> functions = new ArrayList<>();
		FidService fid = new FidService();
		for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
			if (monitor.isCancelled()) {
				return;
			}
			if (only && !listed.contains(f.getEntryPoint())) {
				continue;
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
				listed.add(address);
				continue;
			}
			if (getInstructionAt(address) == null) {
				disassemble(address);
			}
			if (createFunction(address, null) != null) {
				listed.add(address);
				created++;
			}
			else {
				println("RecExport: no function could be made at " + address);
			}
		}
		analyzeChanges(currentProgram);
		println("RecExport: created " + created + " functions");
	}

	/**
	 * Gives near calls through memory the targets a near call has: each word the call reads (Ghidra's
	 * read references from it: a table's entries) as an offset in the caller's code segment, which a near
	 * call can't leave. Makes functions there, and removes the functions Ghidra made at the wrong targets
	 * when nothing else refers to them.
	 */
	private void fixNearCalls() throws Exception {
		if (!(currentProgram.getMinAddress() instanceof SegmentedAddress)) {
			return;
		}
		ReferenceManager refs = currentProgram.getReferenceManager();
		List<Address> right = new ArrayList<>();
		List<Address> wrong = new ArrayList<>();
		InstructionIterator instructions = currentProgram.getListing().getInstructions(true);
		while (instructions.hasNext()) {
			Instruction ins = instructions.next();
			// CALL is near; a far call (CALLF) names its segment.
			if (!ins.getFlowType().isCall() || !ins.getFlowType().isComputed() || !ins.getMnemonicString().equals("CALL")) {
				continue;
			}
			SegmentedAddress at = (SegmentedAddress) ins.getAddress();
			SegmentedAddressSpace space = (SegmentedAddressSpace) at.getAddressSpace();
			// What a near call through memory goes to: the words it reads, in its segment.
			List<Address> targets = new ArrayList<>();
			for (Reference r : ins.getReferencesFrom()) {
				if (r.getReferenceType().isFlow() || !r.getReferenceType().isRead()) {
					continue;
				}
				try {
					Address target = space.getAddress(at.getSegment(), getShort(r.getToAddress()) & 0xFFFF);
					if (!targets.contains(target)) {
						targets.add(target);
					}
				}
				catch (Exception e) {
					// A word Ghidra can't read (uninitialized memory): no target from it.
				}
			}
			if (targets.isEmpty()) {
				continue;
			}
			List<Address> called = new ArrayList<>();
			for (Reference r : ins.getReferencesFrom()) {
				if (!r.getReferenceType().isCall()) {
					continue;
				}
				if (targets.contains(r.getToAddress())) {
					called.add(r.getToAddress());
				}
				else {
					refs.delete(r);
					wrong.add(r.getToAddress());
				}
			}
			for (Address target : targets) {
				if (!called.contains(target)) {
					refs.addMemoryReference(at, target, RefType.COMPUTED_CALL, SourceType.ANALYSIS, 0);
					right.add(target);
				}
			}
		}
		for (Address target : right) {
			if (getFunctionAt(target) == null) {
				disassemble(target);
				createFunction(target, null);
			}
			// Exported with the functions added, whose calls they are.
			listed.add(target);
		}
		int removed = 0;
		for (Address to : wrong) {
			Function f = getFunctionAt(to);
			if (f != null && !refs.getReferencesTo(to).hasNext() && !right.contains(to)) {
				removeFunction(f);
				removed++;
			}
		}
		if (!right.isEmpty()) {
			analyzeChanges(currentProgram);
		}
		println("RecExport: " + right.size() + " near calls moved to their code segment, " + removed
			+ " functions at the wrong targets removed");
	}

	/** Gives the functions the file lists their register parameters and returns (custom storage). */
	private void applySignatures(Path file) throws Exception {
		int applied = 0;
		for (String line : Files.readAllLines(file)) {
			String[] parts = line.trim().split(" ");
			if (parts.length != 3) {
				continue;
			}
			Function f = getFunctionAt(toAddr(Long.parseLong(parts[0])));
			if (f == null) {
				continue;
			}
			try {
				List<Variable> params = new ArrayList<>();
				if (!parts[1].equals("-")) {
					for (String name : parts[1].split(",")) {
						Register r = currentProgram.getRegister(name);
						if (r != null) {
							params.add(new ParameterImpl(name.toLowerCase(), sized(r.getMinimumByteSize()), r,
								currentProgram));
						}
					}
				}
				for (Parameter p : f.getParameters()) {
					if (p.isStackVariable()) {
						params.add(new ParameterImpl(p.getName(), p.getDataType(), p.getVariableStorage(),
							currentProgram));
					}
				}
				Parameter old = f.getReturn();
				ReturnParameterImpl ret =
					new ReturnParameterImpl(old.getDataType(), old.getVariableStorage(), currentProgram);
				if (!parts[2].equals("-")) {
					ret = result(f, parts[2].split("\\+"));
				}
				f.updateFunction(f.getCallingConventionName(), ret, params, FunctionUpdateType.CUSTOM_STORAGE, true,
					SourceType.ANALYSIS);
				applied++;
			}
			catch (Exception e) {
				println("RecExport: no signature for " + f.getEntryPoint() + ": " + e.getMessage());
			}
		}
		println("RecExport: " + applied + " signatures");
	}

	/**
	 * The return of results stored in registers: one ("AX", "DX:AX" a long, "CF" a boolean), or several
	 * as a structure of them, one field a result in order, stored most significant (the last) first.
	 */
	private ReturnParameterImpl result(Function f, String[] results) throws Exception {
		List<Register> storage = new ArrayList<>();
		StructureDataType fields =
			new StructureDataType(new CategoryPath("/rrc"), "results_" + f.getEntryPoint().toString().replace(':', '_'), 0);
		for (String result : results) {
			String[] regs = result.split(":");
			int size = 0;
			for (String name : regs) {
				Register r = currentProgram.getRegister(name);
				if (r == null) {
					throw new Exception("no register " + name);
				}
				size += r.getMinimumByteSize();
			}
			boolean flag = result.endsWith("F") && regs.length == 1;
			fields.add(flag ? BooleanDataType.dataType : sized(size), size,
				result.replace(":", "_").toLowerCase(), null);
			for (String name : regs) {
				storage.add(currentProgram.getRegister(name));
			}
		}
		DataType type;
		if (results.length == 1) {
			type = fields.getComponent(0).getDataType();
		}
		else {
			type = currentProgram.getDataTypeManager()
					.addDataType(fields, DataTypeConflictHandler.REPLACE_HANDLER);
			// Most significant first: the structure's last field is the first piece.
			List<Register> pieces = new ArrayList<>();
			for (int i = results.length - 1; i >= 0; i--) {
				for (String name : results[i].split(":")) {
					pieces.add(currentProgram.getRegister(name));
				}
			}
			storage = pieces;
		}
		return new ReturnParameterImpl(type,
			new VariableStorage(currentProgram, storage.toArray(new Register[0])), currentProgram);
	}

	private static DataType sized(int bytes) {
		return switch (bytes) {
			case 1 -> ByteDataType.dataType;
			case 4 -> DWordDataType.dataType;
			default -> WordDataType.dataType;
		};
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
