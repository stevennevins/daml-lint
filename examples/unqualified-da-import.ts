// Unqualified imports of DA collection modules pull names like `lookup`,
// `insert`, and `null` into scope, shadowing Prelude. Exercises on_import.
// Compile: npx esbuild unqualified-da-import.ts --outfile=unqualified-da-import.js

const NAME = "unqualified-da-import";
const SEVERITY = "low";
const DESCRIPTION = "DA collection modules should be imported qualified";

const SHADOW_PRONE = ["DA.Map", "DA.TextMap", "DA.Set", "DA.List", "DA.Optional"];

function on_import(imp: Import): void {
  if (!imp.qualified && SHADOW_PRONE.includes(imp.module_name)) {
    report(imp, `Import '${imp.module_name}' unqualified — its names shadow Prelude; use 'import qualified ${imp.module_name}'`);
  }
}
