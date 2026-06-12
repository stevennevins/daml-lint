// Templates storing raw ContractId fields (or Optional/List of them) risk
// stale references: the pointed-to contract can be archived underneath.
// Exercises on_field with deep DamlType narrowing.
// Compile: npx esbuild no-bare-contractid-field.ts --outfile=no-bare-contractid-field.js

const NAME = "no-bare-contractid-field";
const SEVERITY = "low";
const DESCRIPTION = "Template fields holding ContractIds risk dangling references";

function containsContractId(ty: DamlType): boolean {
  if (typeof ty === "string") {
    return false;
  }
  if ("ContractId" in ty) {
    return true;
  }
  if ("List" in ty) {
    return containsContractId(ty.List);
  }
  if ("Optional" in ty) {
    return containsContractId(ty.Optional);
  }
  if ("TextMap" in ty) {
    return containsContractId(ty.TextMap);
  }
  return false;
}

function on_field(field: Field, template: Template): void {
  if (containsContractId(field.type_)) {
    report(
      field,
      `Field '${field.name}' on template '${template.name}' stores a ContractId — consider a contract key lookup to avoid dangling references`
    );
  }
}
