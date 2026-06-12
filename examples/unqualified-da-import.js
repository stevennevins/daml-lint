// Compiled from unqualified-da-import.ts — this is the file you pass to --rules.

const NAME = "unqualified-da-import";
const SEVERITY = "low";
const DESCRIPTION = "DA collection modules should be imported qualified";

const SHADOW_PRONE = ["DA.Map", "DA.TextMap", "DA.Set", "DA.List", "DA.Optional"];

function on_import(imp) {
  if (!imp.qualified && SHADOW_PRONE.includes(imp.module_name)) {
    report(imp, `Import '${imp.module_name}' unqualified — its names shadow Prelude; use 'import qualified ${imp.module_name}'`);
  }
}
