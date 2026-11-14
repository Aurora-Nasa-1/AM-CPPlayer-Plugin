// Regenerates src/models.rs and src/operations.rs from the CPPlayer contract.
//
// The provider contract is the single source of truth for record layouts and
// operation tags. This script copies no layout by hand: it walks the schema,
// keeps only what the implemented operations reach, and emits the same
// positional codec the host uses.
//
//   node tools/generate_sdk.mjs
//   CPPLAYER_CONTRACT_DIR=../../CPPlayer-KMP/provider-contract node tools/generate_sdk.mjs
import { writeFileSync, existsSync } from "node:fs";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const contractDir = resolve(
  process.env.CPPLAYER_CONTRACT_DIR ?? join(root, "..", "CPPlayer-KMP", "provider-contract"),
);
const schemaPath = join(contractDir, "schema.mjs");
if (!existsSync(schemaPath)) {
  console.error(`Contract schema not found: ${schemaPath}`);
  console.error("Set CPPLAYER_CONTRACT_DIR to the CPPlayer provider-contract directory.");
  process.exit(1);
}

const loaded = await import(pathToFileURL(schemaPath).href);
const schema = loaded.default ?? loaded.schema;

// Operations this provider implements. Everything else falls back to
// Unsupported on the host, so its records are not generated.
const IMPLEMENTED = [
  "capabilities",
  "search",
  "get",
  "getMany",
  "resolve",
  "resolveDrm",
  "exchangeLicense",
  "getLyrics",
  "searchPlaylists",
  "getPlaylist",
  "getPlaylistTracks",
  "getUserPlaylists",
  "searchAlbums",
  "getAlbum",
  "searchArtists",
  "getArtist",
  "getArtistTracks",
  "getArtistAlbums",
  "getCurrentAccount",
];

function baseType(t) {
  let name = t;
  while (name.endsWith("?") || name.endsWith("[]")) {
    name = name.endsWith("?") ? name.slice(0, -1) : name.slice(0, -2);
  }
  return name;
}

const RECORD_PRIMITIVES = new Set(["string", "int", "long", "bool", "bytes"]);

function reachableRecords() {
  const keep = new Set();
  const visit = (types) => {
    for (const type of types) {
      const record = baseType(type);
      if (RECORD_PRIMITIVES.has(record) || keep.has(record)) continue;
      const fields = schema.records[record];
      if (!fields) throw new Error(`Unknown record referenced by contract: ${record}`);
      keep.add(record);
      visit(Object.values(fields));
    }
  };
  for (const name of IMPLEMENTED) {
    const operation = schema.operations[name];
    if (!operation) throw new Error(`Unknown operation in schema: ${name}`);
    visit([operation.request, operation.response]);
  }
  return keep;
}

const records = reachableRecords();

function rustType(t) {
  if (t.endsWith("?")) return `Option<${rustType(t.slice(0, -1))}>`;
  if (t.endsWith("[]")) return `Vec<${rustType(t.slice(0, -2))}>`;
  switch (t) {
    case "string":
      return "String";
    case "int":
      return "i32";
    case "long":
      return "i64";
    case "bool":
      return "bool";
    case "bytes":
      return "Vec<u8>";
    default:
      return t;
  }
}

const RUST_KEYWORDS = new Set([
  "type", "match", "move", "use", "ref", "in", "as", "box", "fn", "mod",
  "struct", "enum", "where", "for", "loop", "while", "if", "else", "return",
  "break", "continue", "const", "static", "mut", "pub", "crate", "super",
  "self", "Self", "true", "false", "let",
]);

function rustFieldName(name) {
  return RUST_KEYWORDS.has(name) ? `r#${name}` : name;
}

function rustWrite(t, access) {
  if (t === "string") return `w.string(&${access});`;
  if (t === "string?") return `w.optional(${access}.as_deref(), |w, s| w.string(s));`;
  if (t === "string[]") return `w.list(&${access}, |w, s| w.string(s));`;
  if (t === "int") return `w.int(${access});`;
  if (t === "int?") return `w.optional(${access}.as_ref(), |w, &v| w.int(v));`;
  if (t === "long") return `w.long(${access});`;
  if (t === "long?") return `w.optional(${access}.as_ref(), |w, &v| w.long(v));`;
  if (t === "bool") return `w.bool(${access});`;
  if (t === "bool?") return `w.optional(${access}.as_ref(), |w, &v| w.bool(v));`;
  if (t === "bytes") return `w.bytes(&${access});`;
  if (t === "bytes?") return `w.optional(${access}.as_deref(), |w, b| w.bytes(b));`;
  if (t.endsWith("[]")) return `w.list(&${access}, |w, item| item.write(w));`;
  if (t.endsWith("?")) return `w.optional(${access}.as_ref(), |w, item| item.write(w));`;
  return `${access}.write(w);`;
}

function rustRead(t) {
  if (t === "string") return "r.string()?";
  if (t === "string?") return "r.optional(|r| r.string())?";
  if (t === "string[]") return "r.list(|r| r.string())?";
  if (t === "int") return "r.int()?";
  if (t === "int?") return "r.optional(|r| r.int())?";
  if (t === "long") return "r.long()?";
  if (t === "long?") return "r.optional(|r| r.long())?";
  if (t === "bool") return "r.bool()?";
  if (t === "bool?") return "r.optional(|r| r.bool())?";
  if (t === "bytes") return "r.bytes()?";
  if (t === "bytes?") return "r.optional(|r| r.bytes())?";
  if (t.endsWith("[]")) return `r.list(|r| ${baseType(t)}::read(r))?`;
  if (t.endsWith("?")) return `r.optional(|r| ${baseType(t)}::read(r))?`;
  return `${t}::read(r)?`;
}

const HEADER = `// Generated by tools/generate_sdk.mjs from the CPPlayer provider contract.
// Do not edit by hand: field order is the wire layout.
#![allow(dead_code, non_snake_case)]
`;

let models = `${HEADER}
use super::wire::{WireError, WireReader, WireWriter};

`;

for (const [name, fields] of Object.entries(schema.records)) {
  if (!records.has(name)) continue;
  models += `#[derive(Debug, Clone, PartialEq, Default)]\npub struct ${name} {\n`;
  for (const [field, type] of Object.entries(fields)) {
    models += `    pub ${rustFieldName(field)}: ${rustType(type)},\n`;
  }
  models += `}\n\nimpl ${name} {\n`;
  models += `    pub fn write(&self, w: &mut WireWriter) {\n`;
  for (const [field, type] of Object.entries(fields)) {
    models += `        ${rustWrite(type, `self.${rustFieldName(field)}`)}\n`;
  }
  models += `    }\n\n    pub fn read(r: &mut WireReader) -> Result<Self, WireError> {\n`;
  models += `        Ok(Self {\n`;
  for (const [field, type] of Object.entries(fields)) {
    models += `            ${rustFieldName(field)}: ${rustRead(type)},\n`;
  }
  models += `        })\n    }\n}\n\n`;
}

models += `
impl From<WireError> for Failure {
    fn from(error: WireError) -> Self {
        Self {
            category: "InvalidResponse".to_string(),
            message: error.to_string(),
        }
    }
}
`;

writeFileSync(join(root, "src", "models.rs"), models, "utf8");

let operations = `${HEADER}
pub const PROTOCOL_VERSION: i32 = ${schema.version};

pub mod tags {
`;
for (const name of IMPLEMENTED) {
  operations += `    pub const ${name.toUpperCase()}: i32 = ${schema.operations[name].tag};\n`;
}
operations += `}

pub mod capabilities {
`;
for (const name of IMPLEMENTED) {
  const { capability } = schema.operations[name];
  if (capability) operations += `    pub const ${name.toUpperCase()}: &str = "${capability}";\n`;
}
operations += `}

/// Capabilities declared by one session, in contract order.
pub const SUPPORTED: &[&str] = &[
`;
for (const name of IMPLEMENTED) {
  const { capability } = schema.operations[name];
  if (capability) operations += `    capabilities::${name.toUpperCase()},\n`;
}
operations += `];
`;

writeFileSync(join(root, "src", "operations.rs"), operations, "utf8");

console.log(
  `generated ${records.size} records and ${IMPLEMENTED.length} operations from ${schemaPath}`,
);
