// Post-analysis cleanup for Nios II gcc code:
//   1. undisassembled bytes directly after a "ret" are disassembled and made a
//      function if the PseudoDisassembler considers them a valid subroutine
//      (gcc emits functions back to back; functions only reached through
//      pointers are otherwise missed);
//   2. disassembled code that no function covers becomes a function when either
//      the preceding instruction is "ret", or the address is the target of a
//      data/constant reference (function pointers built with movhi/ori|addi,
//      e.g. main passed to __uClibc_main).
// Afterwards the changed program is re-analyzed.  Repeats until nothing changes.
//
// Usage (headless): -postScript Nios2CreateMissingFunctions.java
//
//@category Nios2

import java.util.ArrayList;
import java.util.List;

import ghidra.app.script.GhidraScript;
import ghidra.app.util.PseudoDisassembler;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.ReferenceIterator;

public class Nios2CreateMissingFunctions extends GhidraScript {

	@Override
	protected void run() throws Exception {
		int total = 0;
		for (int pass = 0; pass < 10; pass++) {
			int n = disassembleAfterRet() + onePass();
			total += n;
			if (n == 0) {
				break;
			}
			analyzeChanges(currentProgram);
		}
		println("Nios2CreateMissingFunctions: created " + total + " functions");
	}

	private int disassembleAfterRet() throws Exception {
		Listing listing = currentProgram.getListing();
		PseudoDisassembler pd = new PseudoDisassembler(currentProgram);
		List<Address> cands = new ArrayList<>();
		InstructionIterator it = listing.getInstructions(true);
		while (it.hasNext() && !monitor.isCancelled()) {
			Instruction ins = it.next();
			if (!"ret".equals(ins.getMnemonicString())) {
				continue;
			}
			Address n = ins.getMaxAddress().next();
			if (n == null || !currentProgram.getMemory().getLoadedAndInitializedAddressSet().contains(n)) {
				continue;
			}
			if (listing.getUndefinedDataAt(n) == null) {
				continue; // already code or defined data
			}
			if (pd.isValidSubroutine(n, true)) {
				cands.add(n);
			}
		}
		int created = 0;
		for (Address a : cands) {
			if (listing.getUndefinedDataAt(a) == null) {
				continue;
			}
			if (disassemble(a) && createFunction(a, null) != null) {
				created++;
			}
		}
		return created;
	}

	private int onePass() throws Exception {
		Listing listing = currentProgram.getListing();
		FunctionManager fm = currentProgram.getFunctionManager();
		int created = 0;
		InstructionIterator it = listing.getInstructions(true);
		while (it.hasNext() && !monitor.isCancelled()) {
			Instruction ins = it.next();
			Address a = ins.getAddress();
			if (fm.getFunctionContaining(a) != null) {
				continue;
			}
			boolean start = false;
			Instruction prev = listing.getInstructionBefore(a);
			if (prev != null && prev.getMaxAddress().next() != null &&
				prev.getMaxAddress().next().equals(a) && "ret".equals(prev.getMnemonicString())) {
				start = true;
			}
			if (!start) {
				ReferenceIterator refs = currentProgram.getReferenceManager().getReferencesTo(a);
				while (refs.hasNext()) {
					Reference r = refs.next();
					if (!r.getReferenceType().isFlow()) {
						start = true;
						break;
					}
				}
			}
			if (start && createFunction(a, null) != null) {
				created++;
			}
		}
		return created;
	}
}
