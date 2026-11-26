// Validation helper: linear-sweep every 4-byte word of the executable blocks with
// the Nios II SLEIGH spec (like objdump -d -z) and write one line per word:
//     <hexaddr>\t<A|P|->\t<instruction text>
// A = instruction already in the listing (created by auto-analysis)
// P = decoded on the fly with the PseudoDisassembler (not in listing)
// - = undefined / not decodable
//
// Usage (headless): -postScript DumpNios2Listing.java <outfile>
//
//@category Nios2

import java.io.*;
import java.nio.charset.StandardCharsets;

import ghidra.app.script.GhidraScript;
import ghidra.app.util.PseudoDisassembler;
import ghidra.app.util.PseudoInstruction;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.mem.MemoryBlock;

public class DumpNios2Listing extends GhidraScript {

	@Override
	protected void run() throws Exception {
		String[] args = getScriptArgs();
		File out = new File(args[0]);
		PseudoDisassembler pd = new PseudoDisassembler(currentProgram);
		int nA = 0, nP = 0, nU = 0;
		try (PrintWriter w = new PrintWriter(new OutputStreamWriter(new FileOutputStream(out),
			StandardCharsets.UTF_8))) {
			for (MemoryBlock b : currentProgram.getMemory().getBlocks()) {
				if (!b.isInitialized() || !b.isExecute()) {
					continue;
				}
				for (Address a = b.getStart(); a.compareTo(b.getEnd()) < 0; a = a.add(4)) {
					Instruction ins = currentProgram.getListing().getInstructionAt(a);
					if (ins != null) {
						w.println(Long.toHexString(a.getOffset()) + "\tA\t" + ins.toString());
						nA++;
						continue;
					}
					PseudoInstruction pi = null;
					try {
						pi = pd.disassemble(a);
					}
					catch (Exception e) {
						pi = null;
					}
					if (pi != null) {
						w.println(Long.toHexString(a.getOffset()) + "\tP\t" + pi.toString());
						nP++;
					}
					else {
						w.println(Long.toHexString(a.getOffset()) + "\t-\t");
						nU++;
					}
					if (a.getOffset() + 4 > b.getEnd().getOffset()) {
						break;
					}
				}
			}
		}
		println("DumpNios2Listing: listing=" + nA + " pseudo=" + nP + " undefined=" + nU);
	}
}
