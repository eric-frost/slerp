// Validates GLB and glTF files with the Khronos glTF Validator and prints
// one line of counts per file, then the errors and warnings, or (--json)
// one JSON object per file.  The validator is the `gltf-validator` npm
// package (Apache-2.0), installed in local/ (not in git):
//
//   mkdir -p local/tools/gltf-validator
//   cd local/tools/gltf-validator
//   npm install --ignore-scripts gltf-validator@2.0.0-dev.3.10
//
// Usage: deno run --allow-read tools/validate-gltf.ts [--json] FILE...
// Set GLTF_VALIDATOR to the package folder if it is elsewhere.  Exits 1
// when any file has errors.

const folder = Deno.env.get("GLTF_VALIDATOR") ??
  new URL("../local/tools/gltf-validator/node_modules/gltf-validator", import.meta.url).pathname;
// The package is CommonJS.
import { createRequire } from "node:module";
const validator = createRequire(import.meta.url)(`${folder}/index.js`);

const args = Deno.args.filter((a) => a !== "--json");
const asJson = args.length !== Deno.args.length;
let failed = false;

interface Message {
  code: string;
  message: string;
  severity: number;
  pointer?: string;
}

for (const path of args) {
  let report;
  try {
    const bytes = await Deno.readFile(path);
    report = await validator.validateBytes(bytes, {
      maxIssues: 200,
      // Neighbouring files of .gltf files.
      externalResourceFunction: (uri: string) =>
        Deno.readFile(new URL(uri, `file://${path}`)),
    });
  } catch (e) {
    console.log(asJson ? JSON.stringify({ path, crash: String(e) }) : `${path}: validator failed: ${e}`);
    failed = true;
    continue;
  }
  const issues = report.issues;
  const messages: Message[] = issues.messages;
  if (issues.numErrors > 0) failed = true;
  if (asJson) {
    console.log(JSON.stringify({
      path,
      errors: issues.numErrors,
      warnings: issues.numWarnings,
      infos: issues.numInfos,
      hints: issues.numHints,
      messages: messages.filter((m) => m.severity <= 1).map((m) => `${m.code}: ${m.message} ${m.pointer ?? ""}`),
    }));
    continue;
  }
  console.log(
    `${path}: ${issues.numErrors} errors, ${issues.numWarnings} warnings, ${issues.numInfos} infos, ${issues.numHints} hints`,
  );
  for (const m of messages.filter((m) => m.severity <= 1)) {
    console.log(`  ${m.severity === 0 ? "error" : "warning"} ${m.code}: ${m.message} ${m.pointer ?? ""}`);
  }
}
Deno.exit(failed ? 1 : 0);
