// Applies names from the knowledge files to a program in Ghidra (write-back).
// Run through Ghidra's headless analyzer on an imported program:
//   analyzeHeadless <project> <name> -process <MODULE> -noanalysis -postScript RecApply.java <names.txt>
// Each line of names.txt is "<linear address> <name>", or "<linear address> -" for a function to
// create without a name (found at run time or by prior art). A function is created where Ghidra
// has none, disassembling first; names are set as user-defined, so they win over Ghidra's own.
//@category Rrecreate

import java.nio.file.Files;
import java.nio.file.Path;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.SourceType;

public class RecApply extends GhidraScript {

	@Override
	public void run() throws Exception {
		String[] args = getScriptArgs();
		if (args.length < 1) {
			throw new IllegalArgumentException("usage: RecApply.java <names.txt>");
		}
		int renamed = 0;
		int created = 0;
		int failed = 0;
		for (String line : Files.readAllLines(Path.of(args[0]))) {
			if (line.isBlank()) {
				continue;
			}
			String[] parts = line.trim().split(" ", 2);
			Address address = toAddr(Long.parseLong(parts[0]));
			String name = parts[1];
			Function function = getFunctionAt(address);
			if (function == null) {
				if (getInstructionAt(address) == null) {
					disassemble(address);
				}
				function = createFunction(address, name.equals("-") ? null : name);
				if (function == null) {
					println("RecApply: no function at " + address + " for " + name);
					failed++;
					continue;
				}
				created++;
			}
			if (name.equals("-")) {
				continue;
			}
			if (!function.getName().equals(name) || function.getSymbol().getSource() != SourceType.USER_DEFINED) {
				function.setName(name, SourceType.USER_DEFINED);
				renamed++;
			}
		}
		println("RecApply: renamed " + renamed + ", created " + created + ", failed " + failed);
	}
}
