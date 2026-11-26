// Decompile every function in the program and write the C to a directory:
//   <outdir>/all.c                 everything, in address order
//   <outdir>/funcs/<addr>_<name>.c one file per function
//
// Usage (headless): -postScript ExportNios2Decompiled.java <outdir> [timeoutSecs]
//
//@category Nios2

import java.io.*;
import java.nio.charset.StandardCharsets;

import ghidra.app.decompiler.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionIterator;

public class ExportNios2Decompiled extends GhidraScript {

	@Override
	protected void run() throws Exception {
		String[] args = getScriptArgs();
		if (args.length < 1) {
			printerr("usage: ExportNios2Decompiled.java <outdir> [timeoutSecs]");
			return;
		}
		File out = new File(args[0]);
		int timeout = args.length > 1 ? Integer.parseInt(args[1]) : 60;
		File funcsDir = new File(out, "funcs");
		funcsDir.mkdirs();

		DecompInterface ifc = new DecompInterface();
		DecompileOptions opts = new DecompileOptions();
		ifc.setOptions(opts);
		ifc.toggleCCode(true);
		ifc.toggleSyntaxTree(false);
		ifc.setSimplificationStyle("decompile");
		if (!ifc.openProgram(currentProgram)) {
			printerr("decompiler failed to open program: " + ifc.getLastMessage());
			return;
		}
		int ok = 0, fail = 0;
		try (Writer all = new OutputStreamWriter(new FileOutputStream(new File(out, "all.c")),
			StandardCharsets.UTF_8)) {
			FunctionIterator it = currentProgram.getFunctionManager().getFunctions(true);
			while (it.hasNext() && !monitor.isCancelled()) {
				Function f = it.next();
				if (f.isThunk() || f.isExternal()) {
					continue;
				}
				DecompileResults res = ifc.decompileFunction(f, timeout, monitor);
				String hdr = "// " + f.getName() + " @ " + f.getEntryPoint() + "\n";
				String body;
				if (res != null && res.decompileCompleted() && res.getDecompiledFunction() != null) {
					body = res.getDecompiledFunction().getC();
					ok++;
				}
				else {
					body = "/* decompilation failed: " +
						(res == null ? "null" : res.getErrorMessage()) + " */\n";
					fail++;
				}
				all.write(hdr);
				all.write(body);
				all.write("\n");
				String fname = f.getEntryPoint().toString().replace(':', '_') + "_" +
					f.getName().replaceAll("[^A-Za-z0-9_.-]", "_") + ".c";
				try (Writer w = new OutputStreamWriter(
					new FileOutputStream(new File(funcsDir, fname)), StandardCharsets.UTF_8)) {
					w.write(hdr);
					w.write(body);
				}
			}
		}
		finally {
			ifc.dispose();
		}
		println("ExportNios2Decompiled: " + ok + " ok, " + fail + " failed -> " + out);
	}
}
