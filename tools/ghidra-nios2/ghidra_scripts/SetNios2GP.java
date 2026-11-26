// Find the Nios II global pointer (r26/gp) value and set it as a register value
// over the whole program, so the decompiler and constant-reference analysis can
// resolve gp-relative loads/stores (e.g. "ldw r2,-32748(gp)").
//
// Detection: decodes raw words looking for
//     movhi gp, HI          (orhi  gp, zero, HI)
//     addi  gp, gp, LO      (or ori gp, gp, LO)
// first in the 64 words following each entry point, then (fallback) anywhere in
// executable memory, picking the most common value.
//
// Usage:
//   headless:  -preScript SetNios2GP.java            (auto-detect)
//              -preScript SetNios2GP.java 0x1001bef0 (explicit value)
//   GUI:       Script Manager -> Nios2 -> SetNios2GP
//
//@category Nios2
//@menupath
//@keybinding

import java.math.BigInteger;
import java.util.*;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.lang.Register;
import ghidra.program.model.listing.ProgramContext;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.SymbolTable;

public class SetNios2GP extends GhidraScript {

	private static final int GP = 26;

	@Override
	protected void run() throws Exception {
		Long gp = null;
		String[] args = getScriptArgs();
		if (args.length > 0) {
			gp = Long.decode(args[0]) & 0xffffffffL;
			println("SetNios2GP: using gp from argument: 0x" + Long.toHexString(gp));
		}
		if (gp == null) {
			gp = findFromEntries();
		}
		if (gp == null) {
			gp = findAnywhere();
		}
		if (gp == null) {
			printerr("SetNios2GP: no 'movhi gp / addi gp' sequence found; gp not set");
			return;
		}
		Register reg = currentProgram.getRegister("gp");
		ProgramContext ctx = currentProgram.getProgramContext();
		BigInteger val = BigInteger.valueOf(gp);
		for (MemoryBlock b : currentProgram.getMemory().getBlocks()) {
			if (!b.getStart().getAddressSpace().isMemorySpace()) {
				continue;
			}
			ctx.setValue(reg, b.getStart(), b.getEnd(), val);
		}
		println("SetNios2GP: gp (r26) = 0x" + Long.toHexString(gp) + " set over all memory blocks");
	}

	private Long findFromEntries() throws Exception {
		SymbolTable st = currentProgram.getSymbolTable();
		List<Address> entries = new ArrayList<>();
		AddressIterator it = st.getExternalEntryPointIterator();
		while (it.hasNext()) {
			entries.add(it.next());
		}
		if (entries.isEmpty()) {
			entries.add(currentProgram.getMinAddress());
		}
		for (Address e : entries) {
			Address end;
			try {
				end = e.add(64 * 4);
			}
			catch (AddressOutOfBoundsException ex) {
				end = e.getAddressSpace().getMaxAddress();
			}
			Long v = scan(e, end);
			if (v != null) {
				println("SetNios2GP: found gp setup near entry " + e);
				return v;
			}
		}
		return null;
	}

	private Long findAnywhere() throws Exception {
		Map<Long, Integer> counts = new HashMap<>();
		boolean anyExec = false;
		for (MemoryBlock b : currentProgram.getMemory().getBlocks()) {
			anyExec |= b.isInitialized() && b.isExecute();
		}
		for (MemoryBlock b : currentProgram.getMemory().getBlocks()) {
			if (!b.isInitialized() || (anyExec && !b.isExecute())) {
				continue;
			}
			Address a = b.getStart();
			Long hi = null;
			while (b.getEnd().subtract(a) >= 3) {
				int w;
				try {
					w = readWord(a);
				}
				catch (MemoryAccessException ex) {
					break;
				}
				Long r = step(w, hi);
				if (r != null && (r & 0x100000000L) != 0) {
					long v = r & 0xffffffffL;
					counts.merge(v, 1, Integer::sum);
					hi = null;
				}
				else {
					hi = r;
				}
				try {
					a = a.add(4);
				}
				catch (AddressOutOfBoundsException ex) {
					break;
				}
			}
		}
		Long best = null;
		int bestN = 0;
		for (Map.Entry<Long, Integer> en : counts.entrySet()) {
			if (en.getValue() > bestN) {
				best = en.getKey();
				bestN = en.getValue();
			}
		}
		if (best != null) {
			println("SetNios2GP: gp setup seen " + bestN + "x in memory scan");
		}
		return best;
	}

	private Long scan(Address start, Address end) throws Exception {
		Long hi = null;
		for (Address a = start; a.compareTo(end) < 0; a = a.add(4)) {
			int w;
			try {
				w = readWord(a);
			}
			catch (MemoryAccessException ex) {
				return null;
			}
			Long r = step(w, hi);
			if (r != null && (r & 0x100000000L) != 0) {
				return r & 0xffffffffL;
			}
			if (r != null || hi == null) {
				hi = r;
			}
		}
		return null;
	}

	/**
	 * Returns: the movhi value (no flag) when w is "movhi gp,HI"; the final gp value
	 * with bit 32 set when w completes the pair; null otherwise.
	 */
	private Long step(int w, Long hi) {
		int op = w & 0x3f;
		int a = (w >>> 27) & 0x1f;
		int b = (w >>> 22) & 0x1f;
		int imm = (w >>> 6) & 0xffff;
		if (op == 0x34 && a == 0 && b == GP) { // movhi gp, imm
			return ((long) imm << 16) & 0xffffffffL;
		}
		if (hi != null && a == GP && b == GP) {
			if (op == 0x04) { // addi gp, gp, simm
				return ((hi + (short) imm) & 0xffffffffL) | 0x100000000L;
			}
			if (op == 0x14) { // ori gp, gp, imm
				return ((hi | imm) & 0xffffffffL) | 0x100000000L;
			}
		}
		return null;
	}

	private int readWord(Address a) throws MemoryAccessException {
		return currentProgram.getMemory().getInt(a, false);
	}
}
