import assert from "node:assert/strict";
import test from "node:test";
import { analyzeCommits } from "@semantic-release/commit-analyzer";
import { generateNotes } from "@semantic-release/release-notes-generator";
import { spawnSync } from "node:child_process";
import config from "../release.config.mjs";

const logger = { log() {} };
const pluginOptions = (name) => config.plugins.find(([plugin]) => plugin === name)[1];
for (const [message, expected] of [
  ["fix: correct status refresh", "patch"],
  ["feat: add notifications", "minor"],
  ["feat!: change configuration format", "major"],
  ["fix: change config\n\nBREAKING CHANGE: old config is unsupported", "major"],
  ["docs: clarify installation", null],
  ["chore: update tooling", null],
]) {
  test(`release analysis: ${message.split("\n")[0]}`, async () => {
    assert.equal(await analyzeCommits(pluginOptions("@semantic-release/commit-analyzer"), {
      commits: [{ message }], logger,
    }), expected);
  });
}

test("release notes include features and breaking changes", async () => {
  const notes = await generateNotes(pluginOptions("@semantic-release/release-notes-generator"), {
    options: { repositoryUrl: "https://github.com/example/OwlGood" },
    commits: [{ hash: "a".repeat(40), message: "feat!: new config\n\nBREAKING CHANGE: migrate config" }],
    lastRelease: { gitTag: "1.0.0" },
    nextRelease: { gitTag: "2.0.0", version: "2.0.0" },
    logger,
  });
  assert.match(notes, /2\.0\.0/);
  assert.match(notes, /new config/);
  assert.match(notes, /migrate config/);
});

test("invalid release versions are rejected before building", () => {
  for (const version of ["v1.2.3", "01.2.3", "1.2.3-beta.1", "invalid"]) {
    const result = spawnSync("bash", ["scripts/prepare-release.sh", version], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Invalid release version/);
  }
});

const { appcast, verifyArchive, validateVersion, repository } = await import("./updater-release.mjs");
const { generateKeyPairSync, sign } = await import("node:crypto");
test("updater accepts only stable versions and binds archive URLs to this repository", () => {
  const signature = Buffer.alloc(64).toString("base64");
  assert.match(appcast("1.10.0", signature, 42), /<sparkle:version>1\.10\.0<\/sparkle:version>/);
  assert.ok(appcast("1.10.0", signature, 42).includes(`${repository}/releases/download/1.10.0/OwlGood-1.10.0-macos-universal.app.tar.gz`));
  for (const version of ["v1.0.0", "1.0.0-beta.1", "01.0.0", "1.0.0\n", "../bad", "1.0.0<xml>"]) {
    assert.throws(() => validateVersion(version));
  }
  assert.throws(() => appcast("1.0.0", '" injected="true', 42));
  assert.throws(() => appcast("1.0.0", signature, 0));
});
test("update signatures reject altered archives and wrong keys", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const rawKey = publicKey.export({ type: "spki", format: "der" }).subarray(-32).toString("base64");
  const bytes = Buffer.from("test update archive");
  const signature = sign(null, bytes, privateKey).toString("base64");
  verifyArchive(bytes, signature, rawKey);
  assert.throws(() => verifyArchive(Buffer.from("altered"), signature, rawKey));
  assert.throws(() => verifyArchive(bytes, Buffer.alloc(64).toString("base64"), rawKey));
  const other = generateKeyPairSync("ed25519").publicKey.export({ type: "spki", format: "der" }).subarray(-32).toString("base64");
  assert.throws(() => verifyArchive(bytes, signature, other));
});
test("release refuses a missing signing key before compilation", () => {
  const env = { ...process.env };
  delete env.OWLGOOD_UPDATER_PRIVATE_KEY;
  const result = spawnSync("bash", ["scripts/prepare-release.sh", "1.2.3"], { encoding: "utf8", env });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Missing release configuration/);
});
