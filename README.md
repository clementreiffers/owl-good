# isDown

Application macOS native écrite en Rust pour surveiller des pages de statut depuis la barre de menus. Interface AppKit, sans WebView, réseau via libcurl système et règles compilées avec `regex`.

## Construire et lancer

Prérequis : macOS 13 ou ultérieur, Rust stable (édition 2024) et les Command Line Tools Xcode. Le développement et les essais locaux ont lieu sur Apple Silicon ; une compilation Intel n’équivaut pas à un test sur Mac Intel.

```sh
./scripts/check.sh
./scripts/build-app.sh
open target/release/isDown.app
```

L’application est un accessoire de barre de menus, sans icône permanente dans le Dock. Cliquer son icône pour consulter les services, ouvrir leur page, actualiser, suspendre la surveillance ou accéder aux paramètres. Le bundle est signé localement (ad hoc), pas notarialisé pour distribution publique.

```sh
./scripts/smoke-app.sh
```

Ce contrôle ouvre les composants natifs avec une configuration temporaire et ferme automatiquement l’application. Il ne demande pas l’autorisation des notifications et ne modifie pas les préférences habituelles.

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

Les alias de présentation `Degraded`, `Partial outage`, `Major outage` et `Critical` sont également prévus. L’inventaire et sa fixture sont documentés dans [les fixtures](crates/isdown-monitor/tests/fixtures/README.md). Un vocabulaire ou un code nouveau reste inconnu.

Chaque site possède trois expressions personnalisables. La priorité est **échec > avertissement > succès** ; une expression vide désactive cette catégorie, et aucune correspondance donne un état inconnu. Le moteur Rust `regex` ne prend pas en charge les backreferences ni les look-around. Les expressions sont validées avant sauvegarde et compilées une fois par configuration.

Pour un autre fournisseur, choisir le mode `regex` et renseigner une URL et des expressions adaptées au contenu retourné. Le mode `statusio` attend directement une URL JSON Status.io. Ne pas utiliser une recherche trop générale telle que `Operational` sur une page mêlant plusieurs services et leur historique : définir des règles qui identifient le statut courant.

## Préférences et données

La configuration JSON est enregistrée dans :

`~/Library/Application Support/isDown/config.json`

Les paramètres natifs donnent accès aux options courantes ; l’éditeur avancé expose l’ensemble de la configuration. L’enregistrement est validé et atomique. Une configuration invalide n’est pas remplacée silencieusement par les valeurs par défaut.

Les options couvrent les services, URL, règles, activation, intervalles, timeout, concurrence, péremption, notifications et apparence. Les couleurs utilisent `#RRGGBB` ; les thèmes sont `system`, `light` et `dark`. Les icônes peuvent être remplacées par un fichier local. Le rendu monochrome s’adapte au thème pour rester lisible.

Pour employer un autre fichier, notamment lors de tests :

```sh
ISDOWN_CONFIG_PATH=/tmp/isdown-config.json target/release/isDown.app/Contents/MacOS/isdown
```

Les notifications nécessitent l’autorisation macOS, demandée explicitement depuis les paramètres. Leur politique comporte confirmation, délai anti-répétition, avertissements et rétablissements configurables. Le refus de permission ne bloque pas la surveillance. Pour tester la livraison réelle, lancer le bundle `.app` ; le simple lancement du binaire par Cargo ne garantit pas une identité de notification valide.

## Architecture et contribution

- `isdown-core` : configuration, validation, regex et agrégation pures.
- `isdown-monitor` : collecte réseau et ordonnanceur hors du thread principal.
- `isdown-macos` : AppKit, présentation et notifications.

Les tests automatiques emploient des fixtures, un serveur HTTP local et des politiques pures. Les essais GitLab en ligne ne doivent pas conditionner les tests ordinaires.

Le [plan](PLAN.md) décrit le périmètre cible. Le développement est réparti sur des branches et worktrees dédiés ; un reviewer indépendant contrôle et intègre les contributions. Les intégrations sont séquentielles, tandis que les fonctionnalités indépendantes sont développées en parallèle.
