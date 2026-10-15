# Vérification des mises à jour OwlGood

Le mécanisme reprend le fonctionnement utilisateur d’Ember avec Sparkle pour AppKit : recherche automatique ou manuelle, téléchargement authentifié, installation et redémarrage. Il ne dépend pas de Tauri.

## Clé de publication

La clé privée dédiée est conservée dans le Trousseau macOS sous le compte `com.owlgood.app` par `generate_keys` de Sparkle. Son export alimente le secret GitHub Actions `OWLGOOD_UPDATER_PRIVATE_KEY`. La clé publique est dans `resources/updater-public-key.txt` et injectée dans le bundle. Ne jamais régénérer cette clé à chaque release : les applications déjà distribuées font confiance à la clé existante. Conserver une sauvegarde privée dans un gestionnaire de secrets ; GitHub ne permet pas de relire la valeur d’un secret.

`prepare-release.sh` refuse une clé absente, vérifie que la signature de l’archive correspond à la clé publique versionnée, signe le flux après avoir fixé la version et son URL, puis vérifie ce flux avec l’outil Sparkle. Les checksums sont calculés après signature. Le SDK est épinglé avec son empreinte dans `fetch-sparkle.sh`.

## Vérifications automatisées

- `./scripts/check.sh` : Rust, Clippy et tests du workspace.
- `npm test` : versionnement, schéma du flux, signature valide, clé différente, archive altérée et absence de clé de publication.
- `./scripts/build-app.sh` puis `./scripts/smoke-app.sh` : signatures imbriquées et chargement/démarrage de Sparkle depuis le bundle, avec vérification automatique désactivée par argument pour éviter le réseau pendant le test.
- `scripts/prepare-release.sh` avec une clé privée fournie par environnement : compilation universelle, DMG, contrôle des architectures, extraction et signature du bundle transporté, signature Ed25519 de l’archive et du flux. Cette commande ne publie rien.

## Acceptation sur macOS

Utiliser deux versions N et N+1 sur un compte de test, un flux HTTPS de test et une clé temporaire dédiée. Ne pas modifier le flux stable ni publier une fausse version pour ces essais.

1. Télécharger le DMG N dans un navigateur en conservant la quarantaine. Installer dans Applications. Si Gatekeeper bloque, autoriser explicitement OwlGood dans Confidentialité et sécurité. La signature ad hoc ne garantit pas un premier lancement sans avertissement.
2. Configurer des services, puis proposer N+1 sur le flux de test. Vérifier la recherche manuelle et la proposition automatique, sans opérations concurrentes lors de clics répétés.
3. Télécharger et installer depuis la fenêtre Sparkle. Vérifier le redémarrage, une seule icône de barre de menus, la version N+1 et la conservation des préférences.
4. Recommencer avec une archive altérée, une mauvaise signature, une version de flux modifiée après signature, un flux indisponible et une interruption réseau. Aucun contenu non authentifié ne doit remplacer l’application.
5. Vérifier les erreurs depuis un volume DMG, les permissions d’écriture refusées et l’annulation de l’installation. L’application précédente doit rester utilisable après échec de l’installation.
6. Répéter sur Apple Silicon et Intel. La compilation universelle et les smoke tests ne prouvent pas une mise à jour complète sur les deux architectures.

Les tests locaux ne prouvent ni l’acceptation Gatekeeper d’un téléchargement navigateur, ni un remplacement et redémarrage N → N+1 en production.
