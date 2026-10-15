# OwlGood

**Vos services à l’œil.** Une petite chouette dans votre barre de menus pour suivre vos services et vous prévenir des incidents.

Application macOS native écrite en Rust pour surveiller des pages de statut depuis la barre de menus. Interface AppKit, sans WebView, réseau via libcurl système et règles compilées avec `regex`.

## Construire et lancer

Prérequis : macOS 13 ou ultérieur, Rust stable (édition 2024) et les Command Line Tools Xcode. Le développement et les essais locaux ont lieu sur Apple Silicon ; une compilation Intel n’équivaut pas à un test sur Mac Intel.

```sh
./scripts/check.sh
./scripts/build-app.sh
open target/release/OwlGood.app
```

Pour développer sans recréer le bundle release :

```sh
cargo run --locked
```

Cargo réutilise la compilation incrémentale du profil dev. Ce lancement ne fournit pas l’identité du bundle nécessaire aux notifications macOS.

L’application est un accessoire de barre de menus, sans icône permanente dans le Dock. Cliquer son icône pour consulter les services, ouvrir leur page, actualiser, suspendre la surveillance ou accéder aux paramètres. Le bundle est signé localement (ad hoc), pas notarialisé pour distribution publique.

```sh
./scripts/smoke-app.sh
```

Ce contrôle ouvre les composants natifs avec une configuration temporaire et ferme automatiquement l’application. Il ne demande pas l’autorisation des notifications et ne modifie pas les préférences habituelles.

## CI et releases

La CI GitHub Actions vérifie les tests de versionnement, le formatage Rust, Clippy et les tests du workspace sur macOS à chaque pull request et push sur `main`. Après validation d’un push sur `main`, semantic-release calcule la prochaine version à partir des Conventional Commits :

- `fix:` produit une version patch ; `feat:` une version mineure.
- `!` ou un paragraphe `BREAKING CHANGE:` produit une version majeure.
- `docs:`, `chore:` et les autres commits sans changement publiable ne déclenchent pas de release.

Comme dans `litellm-ember`, les tags sont au format `1.2.3`, sans préfixe `v`, et la première release est `1.0.0`. Les notes sont générées automatiquement. La version calculée est injectée dans les deux champs de version du bundle macOS ; les manifestes Cargo gardent leur version de développement, sans commit automatique de versionnement.

Chaque GitHub Release contient un DMG, une archive `OwlGood-VERSION-macos-universal.app.tar.gz` et `SHA256SUMS.txt`. Le binaire est compilé en profil release pour Apple Silicon et Intel, puis assemblé avec `lipo`. Le packaging vérifie les architectures, la version du bundle, la signature ad hoc et l’intégrité du DMG. L’application reste non notarialisée, comme le build local.

Le workflow utilise uniquement le secret automatique `GITHUB_TOKEN` avec `contents: write` pour publier. Aucun secret Tauri n’est nécessaire pour cette application AppKit. Le dépôt GitHub doit avoir Actions activé et autoriser la création de tags et de releases par ce token.

Pour vérifier le packaging localement sans publier :

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
bash scripts/prepare-release.sh 1.0.0
```

Les fichiers sont déposés dans `release/`. Pour vérifier les règles de versionnement : `npm ci && npm test` (Node.js 24.10 ou ultérieur).

## Surveillance

GitLab est proposé par défaut avec un rafraîchissement de **30 secondes**. Il peut être modifié, désactivé ou supprimé ; une liste vide reste vide au redémarrage.

- Vert : service opérationnel.
- Orange : maintenance, dégradation ou interruption partielle.
- Rouge : interruption majeure ou événement de sécurité.
- Gris : résultat inconnu, données périmées ou surveillance suspendue.

Une erreur de collecte ne prouve pas une panne du fournisseur. Le panneau indique la cause du résultat et son ancienneté. La barre de menus affiche la gravité maximale ; des résultats inconnus empêchent un état entièrement vert.

Le moteur borne la concurrence, les délais et les tailles de réponse. Une modification de configuration invalide les résultats antérieurs encore en vol. Les erreurs répétées espacent les tentatives.

## GitLab et regex

L’exemple utilise l’API **Status.io** réellement exposée par GitLab :

`https://status.gitlab.com/1.0/status/5b36dc6502d06804c08349f7`

L’URL publique reste `https://status.gitlab.com/`. Les regex s’appliquent aux libellés des états **courants**, et non au texte de tous les incidents historiques.

| Catégorie | États GitLab/Status.io reconnus par défaut |
|---|---|
| Succès | Operational |
| Avertissement | Maintenance, Under Maintenance, Planned Maintenance, Degraded Performance, Partial Service Disruption |
| Échec | Service Disruption, Security Event |

Les alias de présentation `Degraded`, `Partial outage`, `Major outage` et `Critical` sont également prévus. L’inventaire et sa fixture sont documentés dans [les fixtures](crates/owlgood-monitor/tests/fixtures/README.md). Un vocabulaire ou un code nouveau reste inconnu.

Chaque site possède trois expressions personnalisables. La priorité est **échec > avertissement > succès** ; une expression vide désactive cette catégorie, et aucune correspondance donne un état inconnu. Le moteur Rust `regex` ne prend pas en charge les backreferences ni les look-around. Les expressions sont validées avant sauvegarde et compilées une fois par configuration.

Pour un autre fournisseur, choisir le mode `regex` et renseigner une URL et des expressions adaptées au contenu retourné. Le mode `statusio` attend directement une URL JSON Status.io. Ne pas utiliser une recherche trop générale telle que `Operational` sur une page mêlant plusieurs services et leur historique : définir des règles qui identifient le statut courant.

## Identité visuelle

OwlGood associe « owl » (chouette) et « all good ». Sa chouette vectorielle à grands yeux veille sur vos services. L’icône monochrome de la barre de menus suit le contraste macOS ; la pastille séparée indique l’état réel, y compris les incidents. Le dessin réutilisable est dans `resources/owlgood-menubar.svg` ; son équivalent AppKit est dessiné à la résolution de l’écran.

## Préférences et données

La configuration JSON est enregistrée dans le dossier Application Support de l’utilisateur, sous OwlGood. Son emplacement est déterminé à l’exécution ; aucun chemin propre à une machine n’est requis.

Les crates et le binaire utilisent le nom `owlgood`, le bundle utilise `com.owlgood.app`, et les variables d’environnement sont préfixées `OWLGOOD_`. Seule la configuration OwlGood est chargée ; aucune migration d’une ancienne application n’est effectuée.

Les réglages natifs regroupent Services, Apparence et Notifications dans une barre latérale, avec des interrupteurs macOS. Les règles de détection et la personnalisation détaillée sont repliées par défaut ; le menu des services donne accès à l’éditeur JSON. Un aperçu permet de comparer les thèmes et le rendu des icônes.

Le panneau et la navigation utilisent le matériau Liquid Glass natif sur macOS 26 ou ultérieur, avec un repli sur les matériaux AppKit sur macOS 13 à 15. Les matériaux système gèrent les préférences d’accessibilité, sans transparence artificielle ajoutée. Le panneau compact fait 300 px de large par défaut, avec des lignes de 44 px. Le nom, le statut et la date/heure locale du dernier contrôle sont regroupés ; la pastille chevauche le coin inférieur droit de l’icône. Sous les sites, une liste de commandes avec icônes et libellés affiche « Paramètres », Actualiser, Suspendre/Reprendre et Quitter ; les détails restent disponibles au survol. Les commandes occupent des lignes compactes de 22 px. Un site sans observation disponible indique « Pas de résultat » ; l’heure affichée correspond au dernier contrôle terminé, même en cas d’erreur réseau. Les réglages partagent un fond continu avec les boutons de fenêtre natifs.

Les interrupteurs et menus appliquent et enregistrent les changements immédiatement. Les champs texte sont enregistrés après une courte pause de saisie ou à la fin de l’édition. Une valeur invalide reste signalée sans changer la configuration active ni déplacer le curseur. Les écritures sont validées et atomiques. L’éditeur JSON conserve une validation explicite ; les changements du formulaire préservent son brouillon. Changer de service ou quitter demande confirmation uniquement si des modifications invalides ou un brouillon JSON restent à traiter.

Les options couvrent les services, URL, règles, activation, intervalles, timeout, concurrence, péremption, notifications et apparence. Les couleurs utilisent `#RRGGBB` ; les thèmes sont `system`, `light` et `dark`. Les icônes peuvent être remplacées par un fichier local. Le rendu monochrome s’adapte au thème pour rester lisible. L’icône de la barre des menus est une image template native : macOS ajuste son contraste au fond de la menubar, indépendamment du thème de l’application. Sa pastille colorée est rendue séparément.

Pour employer un autre fichier, notamment lors de tests :

```sh
OWLGOOD_CONFIG_PATH=owlgood-config.json target/release/OwlGood.app/Contents/MacOS/owlgood
```

Le bouton « Tester les notifications » des paramètres envoie une notification réelle, sans modifier les états des services. Il fonctionne même si les alertes automatiques sont désactivées et utilise le réglage Son du formulaire. Il demande l’autorisation macOS si nécessaire et affiche le résultat de l’envoi ; l’absence de bannière peut dépendre des réglages macOS ou du mode Concentration.

Les notifications nécessitent l’autorisation macOS, demandée explicitement depuis les paramètres. Leur politique comporte confirmation, délai anti-répétition, avertissements et rétablissements configurables. Le refus de permission ne bloque pas la surveillance. Pour tester la livraison réelle, lancer le bundle `.app` ; le simple lancement du binaire par Cargo ne garantit pas une identité de notification valide.

## Architecture et contribution

- `owlgood-core` : configuration, validation, regex et agrégation pures.
- `owlgood-monitor` : collecte réseau et ordonnanceur hors du thread principal.
- `owlgood-macos` : AppKit, présentation et notifications.

Les tests automatiques emploient des fixtures, un serveur HTTP local et des politiques pures. Les essais GitLab en ligne ne doivent pas conditionner les tests ordinaires.

Le [plan](PLAN.md) décrit le périmètre cible. Le développement est réparti sur des branches et worktrees dédiés ; un reviewer indépendant contrôle et intègre les contributions. Les intégrations sont séquentielles, tandis que les fonctionnalités indépendantes sont développées en parallèle.
