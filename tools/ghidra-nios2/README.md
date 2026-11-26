# ghidra-nios2: Altera Nios II (R1) processor module for Ghidra 12

This is a SLEIGH processor module for **Nios II R1** ("Nios II classic"), 32-bit,
little endian. It gives Ghidra a disassembler, decompiler semantics, a calling
convention, an ELF loader opinion for `e_machine` 113 (`EM_ALTERA_NIOS2`),
function-start patterns, and some headless helper scripts. It was written for the
Blonder Tongue EQAM-420B firmware (uClinux 2.6.26, gcc 3.4.6). It is not limited
to that firmware.

The language ID is `Nios2:LE:32:default`, and the compiler spec is `default`.

## Layout

```
Module.manifest, extension.properties   Ghidra extension metadata
data/languages/nios2.slaspec            instruction set: encodings + p-code
data/languages/nios2.ldefs|pspec|cspec  language, processor, ABI
data/languages/nios2.opinion            ELF machine 113 -> Nios2:LE:32:default
data/patterns/                          function-start patterns (prologue after ret/br)
ghidra_scripts/SetNios2GP.java          find gp (movhi gp / addi gp) and set r26 everywhere
ghidra_scripts/Nios2CreateMissingFunctions.java  post-analysis: functions after ret / at pointer targets
ghidra_scripts/ExportNios2Decompiled.java        dump decompiled C of all functions
ghidra_scripts/DumpNios2Listing.java    linear-sweep listing dump (used for validation)
build.sh                                compile .sla + install as user extension
nios2-import.sh                         headless import + gp + analysis + optional C export
compare_objdump.py                      diff a DumpNios2Listing dump against objdump -d -z
```

## Build and install

In the commands below, `$REPO` is the path of this checkout.

Run the build from a directory where a `tmp/` subdirectory is acceptable. The
re-shell dev shell points `_JAVA_OPTIONS` tmpdir at `$PWD/tmp/jtmp`.

```sh
cd $REPO/work/ghidra
nix develop ~/re-shell -c $REPO/tools/ghidra-nios2/build.sh
```

`build.sh` does these steps:

1. Copies the module into `$REPO/work/ghidra/build/Nios2`.
   This is the staging directory. The source tree stays free of build output.
2. Compiles `nios2.slaspec` into `nios2.sla` with `$GHIDRA_INSTALL_DIR/support/sleigh`.
3. Copies the result to the Ghidra **user extensions directory**:
   `~/.config/ghidra/ghidra_12.0.2_NIX/Extensions/Nios2`.

Ghidra reads this directory at startup, for both the GUI and `analyzeHeadless`, so
the nix-store install does not need to change. To pick other locations, set
`NIOS2_BUILD_DIR` or `NIOS2_EXT_DIR`.

Run `build.sh` again after any change to the slaspec, the specs, the patterns or
the scripts. The headless analyzer runs the copy of each script inside the
installed extension, and that copy wins over `-scriptPath`.

## Headless import, analysis and C export

```sh
cd $REPO/work/ghidra
# bflt.py-produced ELF (loader + language are picked automatically from e_machine 113)
nix develop ~/re-shell -c $REPO/tools/ghidra-nios2/nios2-import.sh \
    -d out/update_config/decomp \
    $REPO/work/firmware/epcs1/flat/update_config.elf

# raw kernel image (load address 0)
nix develop ~/re-shell -c $REPO/tools/ghidra-nios2/nios2-import.sh \
    -r 0x0 -n kernel $REPO/work/firmware/epcs1/vmlinux.bin
```

`nios2-import.sh` takes these options:

| Option | Meaning | Default |
|---|---|---|
| `-p DIR` | project directory | `$REPO/work/ghidra/projects` |
| `-n NAME` | project name | `nios2` |
| `-d DIR` | export decompiled C: `DIR/all.c` plus `DIR/funcs/<addr>_<name>.c` | none |
| `-l FILE` | write a linear-sweep listing dump, used for validation | none |
| `-g 0xVALUE` | force the gp value instead of detecting it | detect |
| `-r BASE` | raw binary: BinaryLoader at load address `BASE` | ELF |
| `-k` | do not pass `-overwrite` | overwrite |

The script refuses to write the C export under `work/firmware/`.

The script runs these steps in order:

1. Import the file.
2. Run `SetNios2GP.java` as a **pre-script**, before auto-analysis, so that
   constant-reference analysis already sees gp.
3. Run auto-analysis.
4. Run `Nios2CreateMissingFunctions.java`.
5. Run the exports you asked for.

To open the result in the GUI, start `ghidra` and open the project
`work/ghidra/projects/nios2.gpr`.

### gp (r26)

gcc for Nios II reaches small data through gp, for example
`ldw r2,-32748(gp)`. The ELF start stub loads gp with `movhi gp,HI` and then
`addi gp,gp,LO`. `SetNios2GP.java` finds this pair in the first 64 words after
each entry point. If it finds nothing there, it uses the most common pair in
executable memory. It then sets gp as a program-context register value over
every memory block. The decompiler takes register values from the program
context as tracked values, so gp-relative accesses become plain globals such as
`PTR_DAT_10013f04`, and references to them are created.

To use the script in the GUI, open Script Manager, then go to the Nios2 category
and run SetNios2GP. Run it before auto-analysis, or re-run analysis after it.
To force a value, pass it as a script argument, for example
`-preScript SetNios2GP.java 0x1001bef0`.

The gp value is **per program**. Each bFLT application has its own gp, for
example 0x1001bef0 for update_config and 0x10021e90 for mux_sync.

## How the decompiler sees each instruction

- **Calling convention**:
  - Arguments are in r4 to r7. Further arguments are on the stack, starting at
    `0(sp)`; there is no home area.
  - Return values are in r2, or in r3:r2 for 64-bit values.
  - r16 to r23, fp, sp and gp are callee-saved (unaffected).
  - at, r2 to r15 and ra are killed by calls.
- **r0 (`zero`)**: every read is the constant 0. Writes go to the `zero`
  register, which is never read, so they are dead code.
- **call, callr, jmpi, jmp, ret, br, b\***:
  - `call` and `callr` set ra and are CALL operations.
  - `call` and `jmpi` jump to `(pc & 0xF0000000) | imm26<<2`.
  - `ret`, and `jmp ra`, are RETURN.
  - `jmp rX` is an indirect branch.
  - Branch targets are `pc + 4 + simm16`.
- **eret / bret**: restore status from estatus or bstatus, then return through
  ea or ba.
- **trap n**: modeled as the uClinux 2.6 nios2nommu syscall,
  `r2 = trap(n, r3 /*nr*/, r4, r5, r6, r7)`. Arguments 5 and 6 (r8 and r9) are
  not modeled.
- **break n**: becomes `breakpoint(n)`.
- **rdctl / wrctl**: read or write named control registers: `status`, `estatus`,
  `bstatus`, `ienable`, `ipending`, `cpuid`, `exception`, `pteaddr`, `tlbacc`,
  `tlbmisc`, `eccinj`, `badaddr`, `config`, `mpubase`, `mpuacc`, `ctl6`, and
  `ctl16` to `ctl31`.
- **Cache operations**: `flushd`, `flushda`, `flushi`, `flushp`, `initd`,
  `initda`, `initi` and `sync` are pcodeops.
- **rdprs / wrprs** are pcodeops.
- **custom N, rC|cC, rA|cA, rB|cB** becomes `dst = custom(N, a, b)`.
- **mulxss / mulxsu / mulxuu** give the high 32 bits of the 64-bit product.
  `div` and `divu` are signed and unsigned division.

The listing shows the same pseudo-ops as objdump: `nop`, `mov`, `movi`, `movui`
and `movhi`. The listing differs from objdump only in format:

- Immediates are shown in hex.
- r30 is named `ba`. objdump calls it `sstatus`.

Encodings that objdump treats as undefined (for example, R-type words with a
non-zero IMM5 field) are also undefined here.

## Validation

To check the module, run the import with `-l`, disassemble the same file with
objdump, and compare the two:

```sh
cd $REPO/work/ghidra
nix develop ~/re-shell -c $REPO/tools/ghidra-nios2/nios2-import.sh \
    -l out/update_config/listing.txt -d out/update_config/decomp \
    $REPO/work/firmware/epcs1/flat/update_config.elf
$REPO/work/tools-build/inst/bin/nios2-elf-objdump -d -z \
    $REPO/work/firmware/epcs1/flat/update_config.elf > out/update_config/objdump.txt
python3 -I $REPO/tools/ghidra-nios2/compare_objdump.py \
    out/update_config/objdump.txt out/update_config/listing.txt
```

`compare_objdump.py` reduces both sides to the mnemonic and a list of operand
values. It then reports every word where the mnemonic or an operand differs.

Results with Ghidra 12.0.2 and binutils 2.42 `nios2-elf-objdump -d -z`, checking
every 4-byte word of the executable sections:

| Target | Words compared | In listing after analysis | Mnemonic mismatches | Operand mismatches |
|---|---|---|---|---|
| update_config.elf .text | 13512 | 13327 | 0 | 0 |
| mux_sync.elf .text | 18560 | 18301 | 0 | 0 |
| vmlinux.bin (raw, base 0, whole image) | 1210332 | 464140 | 0 | 0 |
| test/gen_words.py blob (every OP/OPX, edge and random fields) | 115080 | n/a | 0 | 0 |

To run the synthetic blob test:

```sh
cd $REPO/work/ghidra/test
python3 -I $REPO/tools/ghidra-nios2/test/gen_words.py > words.bin
$REPO/work/tools-build/inst/bin/nios2-elf-objdump -D -z -b binary -m nios2 -EL \
    --adjust-vma=0x10000000 words.bin > words.objdump
cd .. && nix develop ~/re-shell -c ghidra-analyzeHeadless projects nios2test -import test/words.bin \
    -overwrite -noanalysis -loader BinaryLoader -loader-baseAddr 0x10000000 \
    -processor Nios2:LE:32:default -cspec default \
    -postScript DumpNios2Listing.java $PWD/test/words.listing
python3 -I $REPO/tools/ghidra-nios2/compare_objdump.py test/words.objdump test/words.listing
```

## Known limitations

- Only R1 is supported. R2 and CDX are not.
- The `trap` syscall model passes only r3 to r7.
- Code that is reached only through pointers is found with heuristics:
  function-start patterns, and `Nios2CreateMissingFunctions.java`. A few unreached
  blocks can stay undisassembled.
- No ELF relocation handler is included. The bflt.py ELFs have no relocations.
