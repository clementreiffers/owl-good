import { createHash, createPublicKey, verify } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

export const repository = "https://github.com/clementreiffers/owl-good";
export function validateVersion(version) {
  if (typeof version !== "string" || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(version)) {
    throw new Error("Expected a stable X.Y.Z version");
  }
}
export function verifyArchive(bytes, signature, publicKey) {
  const key = Buffer.from(publicKey.trim(), "base64");
  const sig = Buffer.from(signature.trim(), "base64");
  if (!bytes.length || key.length !== 32 || sig.length !== 64) throw new Error("Invalid update artifact or signing key");
  const cryptoKey = createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), key]),
    format: "der", type: "spki",
  });
  if (!verify(null, bytes, cryptoKey, sig)) throw new Error("Update signature verification failed");
}
export function appcast(version, signature, length) {
  validateVersion(version);
  if (!/^[A-Za-z0-9+/]{86}==$/.test(signature) || !Number.isSafeInteger(length) || length <= 0) {
    throw new Error("Invalid signature or archive length");
  }
  return `<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>OwlGood</title>
    <link>${repository}</link>
    <description>Mises à jour stables d’OwlGood</description>
    <language>fr</language>
    <item>
      <title>OwlGood ${version}</title>
      <sparkle:version>${version}</sparkle:version>
      <sparkle:shortVersionString>${version}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
      <description>Une nouvelle version d’OwlGood est disponible.</description>
      <link>${repository}/releases/tag/${version}</link>
      <enclosure url="${repository}/releases/download/${version}/OwlGood-${version}-macos-universal.app.tar.gz" sparkle:edSignature="${signature}" length="${length}" type="application/octet-stream" />
    </item>
  </channel>
</rss>
`;
}
async function main() {
  const [command, version] = process.argv.slice(2);
  validateVersion(version);
  if (command === "notes") {
    console.log(`### Installer et mettre à jour OwlGood

Téléchargez le [DMG universel](${repository}/releases/download/${version}/OwlGood-${version}-macos-universal.dmg) et déplacez OwlGood dans Applications. Pour les versions intégrant Sparkle, utilisez **Paramètres → Mises à jour → Rechercher une mise à jour…**. Les vérifications automatiques sont quotidiennes ; l’installation et le redémarrage demandent votre accord.

L’application est signée ad hoc, sans certificat Developer ID ni notarisation Apple. Le conteneur DMG reste non signé, comme celui d’Ember. Cette signature ne supprime pas la quarantaine ajoutée par le navigateur. Si macOS bloque le premier lancement, utilisez Réglages Système → Confidentialité et sécurité → Ouvrir quand même. Les mises à jour et leur flux sont authentifiés séparément par Ed25519.`);
  } else if (command === "artifacts") {
    const base = `OwlGood-${version}-macos-universal`;
    const archive = await readFile(`release/${base}.app.tar.gz`);
    const signature = (await readFile(`release/${base}.app.tar.gz.sig`, "utf8")).trim();
    verifyArchive(archive, signature, await readFile("resources/updater-public-key.txt", "utf8"));
    await writeFile("release/appcast.xml", appcast(version, signature, archive.length));
  } else if (command === "checksums") {
    const base = `OwlGood-${version}-macos-universal`;
    const files = [`${base}.dmg`, `${base}.app.tar.gz`, `${base}.app.tar.gz.sig`, "appcast.xml"];
    const sums = await Promise.all(files.map(async file => {
      const bytes = await readFile(`release/${file}`);
      if (!bytes.length) throw new Error(`Empty artifact: ${file}`);
      return `${createHash("sha256").update(bytes).digest("hex")}  ${file}\n`;
    }));
    await writeFile("release/SHA256SUMS.txt", sums.join(""));
  } else throw new Error("Expected notes, artifacts or checksums");
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
