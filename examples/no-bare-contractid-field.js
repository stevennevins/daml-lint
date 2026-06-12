// Compiled from no-bare-contractid-field.ts — this is the file you pass to --rules.

const NAME = "no-bare-contractid-field";
const SEVERITY = "low";
const DESCRIPTION = "Template fields holding ContractIds risk dangling references";

function containsContractId(ty) {
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

function on_field(field, template) {
  if (containsContractId(field.type_)) {
    report(
      field,
      `Field '${field.name}' on template '${template.name}' stores a ContractId — consider a contract key lookup to avoid dangling references`
    );
  }
}
