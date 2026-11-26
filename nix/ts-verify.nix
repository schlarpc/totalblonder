# The transport-stream checker (crates/tb-ts-verify).
{
  pkgs,
  lib,
  root,
  tb-ts-verify,
}:
let
  rules = "${
    lib.fileset.toSource {
      inherit root;
      fileset = root + "/notes/ts-conformance-rules.md";
    }
  }/notes/ts-conformance-rules.md";
  bin = "${tb-ts-verify}/bin/tb-ts-verify";
in
{
  checks = {
    # notes/ts-conformance-rules.md carries the catalog the binary prints.
    "ts-verify/rules-catalog" = pkgs.runCommand "ts-verify-rules-catalog" { } ''
      ${bin} --rules-md > generated.md
      sed -n '/^<!-- generated: tb-ts-verify --rules-md -->$/,$p' ${rules} | tail -n +2 > committed.md
      cmp generated.md committed.md
      touch $out
    '';
  };
}
