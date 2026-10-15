# Plan de réalisation — isDown

## 1. Objectif et périmètre

Application macOS écrite en Rust, accessible depuis une icône dans la barre de menus. Un clic ouvre un panneau natif avec les services surveillés, leur icône monochrome sur fond transparent, une pastille et un libellé d’état. La pastille de la barre de menus résume l’état global.

- Vert : opérationnel ; orange : dégradation ; rouge : incident majeur.
- Gris avec libellé explicite : inconnu, données périmées ou surveillance suspendue. Une erreur réseau locale ne prouve pas que le service est en panne.
- Intervalle par défaut : 30 secondes, réglable globalement et par service.
- GitLab (`https://status.gitlab.com/`) proposé au premier lancement, modifiable et supprimable sans réapparition après redémarrage.
- Notifications natives sur transition vers un incident, avec politique réglable.
- Ajout de pages de statut arbitraires, adaptateurs structurés lorsque disponibles et règles regex par service.

Le périmètre initial est macOS. La version minimale et les architectures distribuées seront arrêtées après vérification des bindings et du packaging ; Apple Silicon est la première cible de validation locale.

## 2. Architecture proposée

Utiliser `objc2`, `objc2-app-kit`, `objc2-foundation` et `objc2-user-notifications`, avec uniquement les fonctionnalités nécessaires. `NSStatusItem`, `NSPopover` et une fenêtre de paramètres AppKit donnent de vrais composants macOS. Pas de moteur web embarqué. Les appels natifs et les blocs `unsafe` seront limités aux adaptateurs, avec invariants documentés.

Comparer dans un prototype `NSURLSession` et un client Rust léger. Préférer le réseau natif si annulation, timeouts, tests et maintenance restent simples ; éviter d’ajouter Tokio et une pile TLS uniquement par habitude. Mesurer taille du bundle, mémoire au repos et CPU avant de figer le choix : la légèreté est un objectif vérifié, pas une promesse théorique.

Découpage cible en workspace Cargo :

| Crate | Responsabilité |
|---|---|
| `isdown-core` | États, modèles, classification, agrégation, transitions et décisions pures |
| `isdown-config` | Configuration versionnée, validation, migrations et persistance |
| `isdown-monitor` | Réseau, adaptateurs de pages, règles regex, ordonnanceur |
| `isdown-macos` | AppKit, icônes, paramètres, notifications et lancement de l’application |

Le cœur reçoit des observations et produit un nouvel état et des effets à exécuter. Horloge, transport, stockage et notifications sont injectables. Employer fonctions pures, enums exhaustives, itérateurs et emprunts quand ils améliorent le code ; ne pas imposer de copies ou de couches d’abstraction pour afficher un style fonctionnel.

AppKit reste sur le thread principal. Téléchargements, parsing et traitement des images se font hors de ce thread ; les résultats sont transmis par événements. Un identifiant de génération empêche un résultat ancien de remplacer un résultat récent ou de recréer un service supprimé.

## 3. Règles fonctionnelles

### Surveillance et interprétation

- Distinguer disponibilité de la page de statut et état du service : un HTTP 200 ne suffit jamais à conclure « vert ».
- Pour GitLab, vérifier l’API publique réellement disponible avant de choisir un adaptateur ; conserver une fixture datée et des tests de mapping. Ne pas figer une URL d’API supposée.
- Fournir avec l’exemple GitLab un jeu de regex préconfigurées et activables immédiatement, couvrant tous les états d’échec (rouge), d’avertissement (orange) et de succès (vert) recensés dans les données actuelles de GitLab. Ces règles sont visibles, modifiables, désactivables et restaurables dans les paramètres ; l’utilisateur n’a pas à les écrire lui-même.
- Avant de livrer ces regex, inventorier les valeurs et messages effectivement exposés par GitLab et documenter leur correspondance avec les trois gravités, y compris les pannes partielles, dégradations et maintenances. Appliquer les règles aux champs d’état courants extraits, sans scanner indistinctement l’historique des incidents. Les libellés de progression d’un incident ne suffisent pas à déterminer sa gravité.
- La couverture exhaustive des regex GitLab porte sur cet inventaire vérifié, avec une fixture par cas. Une valeur nouvelle ou non reconnue doit rester « inconnue » et être signalée ; ne pas garantir la reconnaissance de futurs changements de format ou de vocabulaire.
- Séparer les incidents actifs, composants et maintenances des incidents historiques résolus.
- Chaque service choisit son mode : adaptateur structuré ou regex, avec repli explicite si configuré.
- Regex appliquées à un champ choisi : corps brut, texte normalisé ou message extrait. Compiler à la sauvegarde ; indiquer les erreurs et proposer un test sur un échantillon avec résultat et règle correspondante.
- Priorité par défaut en cas de correspondances multiples : échec > avertissement > succès. Priorité et résultat sans correspondance configurables ; absence de correspondance = inconnu par défaut.
- Utiliser le moteur `regex`, sans backreferences/look-around ; expliquer ces limites dans l’éditeur. Borner taille du contenu et des expressions, réutiliser les regex compilées.
- Gérer redirections limitées, TLS, timeouts, réponses trop grosses, HTTP 429/Retry-After, encodages et réponses invalides. Afficher une cause exploitable sans transformer chaque erreur en incident fournisseur.
- Polling concurrent borné, sans chevauchement pour un même service. Refresh manuel, pause globale/par service, reprise après veille et annulation lors d’une suppression.
- Conserver le dernier état connu avec son horodatage ; rendre visible sa péremption. Adapter temporairement les tentatives après erreurs répétées pour éviter une boucle agressive.
- État global : gravité maximale des états connus actifs ; signaler aussi les inconnus/périmés. Tous inconnus ou liste vide ne doivent pas donner une pastille verte.

### Interface et personnalisation

- Panneau : icône, nom, gravité, résumé, dernière vérification, état périmé, ouverture de la page, actualisation et accès aux paramètres.
- Icônes noires sur transparence en thème clair ; rendu template adaptatif en thème sombre pour conserver le contraste. Couleur et mode fixes restent configurables.
- Les favicons récupérées sont mises en cache et converties en masque monochrome ; proposer aperçu, icône locale et symbole de secours, car la conversion automatique peut dégrader certains logos.
- Icône de barre de menus et pastille : symbole, taille dans les limites macOS, couleur, position, visibilité, texte optionnel et aperçu. Aucun état ne repose uniquement sur une couleur.
- Préférences d’affichage : thème système/clair/sombre, densité, dimensions du panneau, taille des icônes, couleurs des états, tri, groupes, champs visibles et noms personnalisés.
- VoiceOver, navigation clavier, contraste et réduction des animations ; respecter les contraintes des composants natifs plutôt que promettre un contrôle illimité de macOS.

### Paramètres

Fenêtre native structurée en Général, Services, Règles, Apparence, Notifications et Avancé. Toutes les options exposées par l’application sont persistées, validées et documentées avec leurs valeurs par défaut et bornes.

- Général : lancement à l’ouverture de session, intervalle, pause et comportement à la reprise.
- Services : ajout/retrait, URL, nom, groupe, ordre, activation, intervalle propre, adaptateur, icône et règles.
- Réseau : timeout, concurrence, péremption, politique de nouvelles tentatives ; validation des bornes.
- Notifications : activation globale/par service, gravité minimale, confirmation après N observations, retour à la normale, son, période silencieuse et délai anti-répétition.
- Apparence : paramètres listés ci-dessus, prévisualisation et restauration des valeurs par défaut.
- Configuration : export/import versionné, validation avant remplacement, réinitialisation explicite et diagnostics sans données sensibles.

Prévoir `serde` et un format lisible à confirmer (TOML proposé), stockage dans Application Support, écriture atomique et récupération après fichier corrompu. Initialisation des exemples une seule fois. L’import et les changements en cours d’exécution ne doivent pas laisser de tâches orphelines.

### Notifications

Employer UserNotifications avec une identité de bundle stable. Expliquer l’usage avant la demande système ; le refus n’empêche pas la surveillance. Déclencher sur transitions confirmées, dédupliquer les répétitions et éviter une avalanche à la reprise ou au démarrage. Notifications de récupération et de dégradation optionnelles. Le clic ouvre le service concerné. Le comportement réel sera validé dans un bundle `.app`, pas uniquement via `cargo run`.

## 4. Organisation multi-agent

Le coordinateur est l’interlocuteur direct de l’utilisateur. Il maintient le backlog, les critères d’acceptation, les contrats et les dépendances ; il répartit les tâches et rend compte des arbitrages. Chaque développeur possède une fonctionnalité, une branche `feat/<lot>` et un worktree `.worktrees/<lot>` distinct. Aucun développeur ne travaille dans le worktree d’un autre.

Un agent reviewer-intégrateur indépendant relit les propositions, demande les corrections au développeur puis intègre les changements validés dans son propre worktree `.worktrees/integration`, branche `integration`. Il n’approuve pas ses propres corrections substantielles : elles repassent chez le développeur pour validation. Les modifications des manifestes partagés et du lockfile passent par lui.

La session permet quatre agents simultanés : coordinateur + reviewer-intégrateur + deux développeurs. On parallélise deux fonctionnalités indépendantes à la fois ; les autres attendent un créneau. « Un développeur par fonctionnalité » désigne une affectation isolée, pas une promesse de lancer tous les lots simultanément malgré leurs dépendances.

Cycle de livraison :

1. Coordinateur : spécification courte, interfaces, fichiers possédés, dépendances et tests attendus.
2. Intégrateur : création de la branche/worktree à partir du dernier socle validé.
3. Développeur : implémentation et tests, commits locaux, rapport avec SHA, limites et preuve des contrôles.
4. Reviewer : revue de code, sécurité mémoire/threads, erreurs, UX et cas limites ; retour au développeur jusqu’à résolution.
5. Intégrateur : fusion séquentielle, résolution des conflits avec l’auteur, vérification de l’ensemble. Les développements sont parallèles ; les fusions restent séquentielles.
6. Coordinateur : validation des critères du lot, compte rendu utilisateur ; intégration de la version validée sur `main`. Nettoyage des worktrees seulement après conservation des commits.

Pas de publication, push ou distribution externe prévu par ce plan. Aucune réécriture destructive du travail utilisateur.

## 5. Lots, dépendances et parallélisation

| Lot | Développeur / branche | Livraison et critères | Dépendances |
|---|---|---|---|
| L0 | `dev-socle` / `feat/socle` | Workspace, contrats, erreurs, fixtures, prototype AppKit/réseau, mesures initiales, bundle minimal | Initialisation |
| L1 | `dev-core` / `feat/core` | États, agrégation, fraîcheur, transitions ; table complète des règles testée | L0 |
| L2 | `dev-config` / `feat/config` | Schéma, valeurs par défaut, exemple GitLab, persistance, migrations/import ; suppression durable | L0 |
| L3 | `dev-monitor` / `feat/monitor` | Adaptateur GitLab vérifié, regex GitLab préconfigurées couvrant l’inventaire des échecs/avertissements/succès, transport et ordonnanceur ; tests réseau locaux et horloge simulée | L1, L2 |
| L4 | `dev-menubar` / `feat/menubar` | Status item, panneau natif, icônes/cache et agrégation affichée ; fonctionne sur observations simulées | L1, L2 |
| L5 | `dev-settings` / `feat/settings` | Fenêtre complète, éditeur regex, aperçu apparence et application dynamique | L3, L4 |
| L6 | `dev-notifications` / `feat/notifications` | Permissions, transitions, anti-spam, retour à la normale et clic | L3, L4 |
| L7 | `dev-release` / `feat/release` | Assemblage final, bundle, accessibilité, reprise/arrêt, mesures et documentation | L5, L6 |

Vagues : L0 → (L1 ∥ L2) → (L3 ∥ L4) → (L5 ∥ L6) → L7. Pour L3/L4, les interfaces d’observation sont figées à la fin de L1, permettant à l’interface de progresser avec un faux transport. Chaque lot est présenté au même reviewer-intégrateur avant admission dans la base des lots suivants.

## 6. Validation

Maximiser les tests unitaires utiles, sans gonfler leur nombre avec des tests de getters. Le cœur et les politiques ont la couverture la plus élevée ; documenter les branches non couvertes. Les frontières AppKit sont vérifiées par tests d’intégration et essais réels.

- Tests tabulaires : gravités, incidents actifs/résolus, maintenances, inconnus, états mixtes, données périmées, liste vide, regex contradictoires/invalides et aucun match.
- Regex GitLab par défaut : un test par état recensé dans chacune des trois catégories, tests de priorité en présence de plusieurs états, absence de faux incident provenant de l’historique résolu, valeur inconnue, et modification/restauration des règles par l’utilisateur. La revue doit vérifier que chaque entrée de l’inventaire possède une règle et un test associés.
- Propriétés : agrégation indépendante de l’ordre, aucune observation périmée appliquée après une observation récente, transitions stables sans notification répétée.
- Transport local simulé : 200 avec incident, erreurs HTTP, timeout, annulation, redirection, limitation et payload invalide. Pas d’Internet requis pour les tests automatiques ordinaires.
- Horloge simulée : cadence 30 s, intervalle personnalisé, absence de chevauchement, veille/reprise, backoff, suppression pendant une requête et arrêt propre.
- Stockage temporaire : aller-retour, migration, corruption, import rejeté, sauvegarde atomique et suppression définitive de GitLab.
- Notifications simulées : seuil, confirmation, refus de permission, silencieux, répétitions, changement de configuration et récupération.
- Tests manuels macOS : vrai panneau, écran Retina, clair/sombre, VoiceOver/clavier, notifications autorisées/refusées, login item, reprise, import/export et paramètres d’apparence.
- Test réel GitLab opt-in pour confirmer le contrat de l’adaptateur ; aucune assertion sur un état « vert » permanent.

À chaque intégration : `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-targets`, `cargo test --workspace --doc` et build release pour les étapes natives. Vérifier le bundle sur macOS. Tester Intel séparément si sa distribution est retenue ; ne pas confondre compilation croisée et validation réelle.

Mesurer sur une machine identifiée, avec 1, 10 et 50 services : taille du bundle, mémoire et CPU au repos/en refresh, latence d’ouverture du panneau. Fixer un budget après L0 et vérifier les régressions en L7.

## 7. État initial et contrainte Git

Le socle actuel est un binaire Cargo en édition 2024, sans dépendances externes. Aucune fonctionnalité produit n’est encore implémentée.

L’initialisation Git a échoué : l’environnement refuse la création de `.git` (chemin protégé en écriture). L’autorisation utilisateur est acquise ; c’est une restriction technique. Aucun dépôt, commit, branche ou worktree n’a donc été créé. Les deux agents consultés pour élaborer ce plan font uniquement de l’analyse sans édition.

Une fois l’écriture Git disponible, initialiser `main`, committer le socle validé, créer `integration` et les worktrees des lots prêts. Ne pas présenter les travaux de développement multi-agent comme démarrés avant cette étape.
