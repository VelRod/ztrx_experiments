# Rapport d'architecture : état actuel et axes d'amélioration

Date : 2026-11-26. Base : `main` à `116465c` plus la branche
`claude/heap-texture-residency`. Les chiffres viennent d'une partie de 7 min dans
le prologue (profil Series X, RTX 5070 12 Go, Ryzen 7 5800X3D, réglages rapides
de `scripts/play.ps1`, build diagnostics), relevés environ 4 min après le
début du monde (partie « v2 », ~4 850 draws par frame), sauf mention contraire.

## 1. En bref

- Le jeu tourne à **~5,5-6 fps** (179 ms par frame). Pour 30 fps il faut
  **33 ms** : il faut diviser le temps de frame par 5 à 6. Aucun réglage fin n'y
  mènera ; il faut changer la façon dont la mémoire du titre arrive à la carte
  et en revient.
- Le coût dominant n'est pas le travail GPU du jeu (au moins 40 ms par frame
  mesurés pour ses commandes les plus lourdes) mais **la cohérence entre la
  mémoire du titre et ses copies côté hôte** : copier ce que le titre écrit
  (57 ms de CPU par frame), surveiller ses écritures (26 ms d'interrogations du
  write-watch, et une part des 58 ms de temps noyau des soumissions), et
  rapatrier ce que les shaders écrivent (147 ms par frame sur le thread de
  recopie, dont 105 ms à attendre la carte).
- Deux threads font la frame à la chaîne : le **thread de soumission** (~100 ms
  de CPU par frame, plus 20 ms d'attente de verrou et 62 ms d'attente de la
  carte) et le **thread de recopie** (147 ms). Cinq threads d'enregistrement du
  titre passent 44 à 49 ms chacun dans nos appels de liste.
- La configuration rapide n'est **pas celle du code par défaut** : elle n'existe
  que dans `scripts/play.ps1` (7 variables d'environnement). Le dépôt compte
  **146 interrupteurs `VICTORY_*`**, dont 100 dans `victory-gpu`.
- La base de tests est sérieuse (plus de 500 tests, tests GPU sur la carte et sur WARP,
  validation DXIL), mais il n'existe **aucun banc de mesure versionné** : chaque
  session réécrit ses scripts, et chaque recompilation invalide le cache de
  shaders (première partie inutilisable pour les fps).

## 2. L'architecture en bref

```
exécutable du jeu ─► victory-loader ─► code du titre (x86-64 natif)
                                          │ imports
                                          ▼
                                       victory-hle (API console → Windows)
                                          │ D3D12.X
                                          ▼
                                       victory-gpu (D3D12.X → D3D12 hôte)
                                          │ shaders à la création des pipelines
                                          ▼
                                       victory-shader (ISA GFX10 → DXIL)
```

| Crate | Lignes | Tests | Rôle |
|---|---:|---:|---|
| `victory-shader` | 42 150 | 221 | traduction GFX10 → SSA → DXIL (écrivain de bitcode maison) |
| `victory-gpu` | 37 918 | 129 | objets D3D12.X, listes, mémoire du titre, soumission, présentation |
| `victory-hle` | 18 065 | 96 | API de la console (mémoire, fichiers, audio, entrées, sauvegardes…) |
| `victory-formats` | 4 365 | 28 | tuilage des textures, codecs audio |
| `victory-loader` | 3 645 | 28 | chargement, liaison, TLS, rapport de plantage |
| autres (`core`, `pe`, `pipeline`, `game`, app, `xtask`) | 3 659 | 25 | configuration, PE, cache, CLI |

Le découpage en crates est sain : chaque couche a un rôle net et les
dépendances descendent. Tout le poids est dans deux crates : `victory-gpu` et
`victory-shader` font 80 % du code.

## 3. Ce qui est solide

- **Tests à plusieurs niveaux.** Le traducteur est testé de l'ISA jusqu'à
  l'exécution : programmes GFX10 encodés à la main, DXIL passé au validateur
  `dxil.dll`, exécution sur la carte et sur WARP, comparaisons avec le
  compilateur PC (`diff_tests`). C'est le bon outil pour un traducteur.
- **Discipline `unsafe`.** `unsafe_op_in_unsafe_fn` est refusé, et les 644 blocs
  `unsafe` de `victory-gpu` portent presque tous un commentaire `SAFETY`.
- **CI utile.** Clippy en `-D warnings` dans les deux configurations, plus les
  tests, sous Windows.
- **Décisions documentées dans le code.** Les commentaires disent pourquoi,
  avec la mesure qui a tranché (« run 45 », « 0,33 ms each »). C'est précieux,
  à condition que ces mesures restent vraies (voir 4.6).
- **Diagnostics riches.** Le journal donne, toutes les 5 s, la ventilation
  complète d'une frame. C'est ce qui permet ce rapport.

## 4. Remarques

### 4.1 Le modèle mémoire est le cœur du coût

Sur la console, CPU et GPU partagent une seule mémoire ; le titre écrit les
données du GPU directement. Sur PC, `victory-gpu` tient des **copies hôte** de
la mémoire du titre (`shadow.rs`, `resource.rs`), repère ce que le titre écrit
(`watch.rs` : write-watch du noyau, gardes en lecture seule sur les vues de
pages physiques, hachage des textures), recopie ces pages avant chaque
soumission, puis rapatrie ce que les shaders ont écrit (comparaison sur la
carte, `fetch`, fusion). Par frame :

| Poste | Mesure |
|---|---|
| Copies de buffers du titre vers leurs copies hôte | 57,4 ms, 122,7 Mo |
| Interrogations du write-watch | 2 666, 26,4 ms, 9,5 Go examinés dont 9,4 Go inchangés |
| Déplacements (`moves`) | 21,6 ms |
| Copies dans la mémoire de la carte | 97 copies, 941 Mo ; 83,4 Mo envoyés en 268 morceaux |
| Rapatriement des écritures des shaders | 17 648 morceaux comparés sur la carte, 1 217 récupérés (78,7 Mo) |
| Thread de recopie | 146,6 ms d'activité, dont **105,1 ms à attendre ces récupérations** |
| Passes de la couche sur la carte | 262 passes de hachage, 97 de comparaison |
| Le plus gros buffer (174 Mo à `0x37820000`) | rafraîchi 15,2 fois par frame |

Chaque poste a déjà été optimisé (fenêtres, gardes, relais, copies sur la
carte, hachage par morceaux), mais le principe impose des allers-retours à
chaque soumission (31 par frame). Le code prépare déjà une alternative :
`inplace.rs` ouvre la mémoire du titre **comme mémoire de la carte**
(`OpenExistingHeapFromAddress`, `OpenExistingHeapFromFileMapping`), ce qui
supprime copie et rapatriement. Cette voie est désactivée
(`VICTORY_INPLACE=0`) et limitée par des contraintes connues : sections créées
`SEC_RESERVE` refusées, pages verrouillées, tas en mémoire PC donc lu à travers
PCIe.

### 4.2 Le thread de soumission est un goulot unique

Un seul thread soumet (31 soumissions par frame). Dans les soumissions, il
consomme 42 ms de temps utilisateur et 57,6 ms de **temps noyau** par frame
(sans doute surtout write-watch, gardes et requêtes mémoire : à ventiler). Il
attend en plus 20,3 ms le verrou de soumission. Entre deux soumissions, il
passe 49,6 ms dont 9 ms seulement à calculer. Les attentes de la carte qu'il
subit (62,4 ms par frame) viennent surtout du plafond de 128 listes en vol
(35,5 ms) et de lectures de mémoire du titre que des écritures de shaders en
vol pourraient changer (24,8 ms).

Note : la colonne « on the GPU » de la ligne de frame est ce temps d'attente,
pas le temps de calcul de la carte. Le nom induit en erreur.

### 4.3 L'enregistrement des listes coûte cher par appel

41 073 appels de liste par frame, 235,7 ms cumulés sur les threads du titre
(cinq threads à 44-49 ms chacun), soit ~5,7 µs par appel. Chaque draw prépare ses
tables, recopie des descripteurs, rafraîchit des plages, cherche des
emplacements dynamiques (12 132 tables par frame). Comme c'est réparti sur
cinq threads, ce n'est pas le premier goulot. Ça le deviendra dès que 4.1 et 4.2
seront réglés : à 30 fps, il ne reste que 33 ms par thread.

### 4.4 Le GPU

La carte a du travail en file ~77 % du temps. Ses commandes les plus lourdes
pèsent au moins 40 ms par frame : 20,5 ms de dispatchs, 15,7 ms de draws
indirects, 2,4 ms de mesh, 1,9 ms de copies (partie instrumentée, top des
commandes seulement). Le reste est le travail de la couche : copies, 359
passes de hachage et de comparaison, 2 506 dispatchs de bornage des commandes
indirectes, 709 attentes sur les groupes de barrières du titre.

Les 340 à 360 dispatchs par frame du constructeur de structures de lancer de
rayons sont **sautés** (`VICTORY_BVH_BUILDER` éteint). C'est la cause des ombres
noires et des surexpositions (`docs/fork-ray-tracing.md`). Les réactiver
ajoutera du travail GPU : il faut le compter dans le budget des 30 fps.

### 4.5 Configuration : 146 interrupteurs, et le chemin rapide n'est pas le défaut

- 146 variables `VICTORY_*` distinctes, lues chacune dans un `OnceLock` près
  de son usage. Il n'existe aucun endroit qui dit la configuration effective
  d'une partie. Certaines interagissent (« ne pas combiner `VICTORY_INPLACE=2`
  avec late wait »).
- Les sept réglages qui font passer de 2,1 à ~6 fps (`VICTORY_RELAY=2`,
  `VICTORY_CARD_BUFFERS_*`…) ne sont activés que par `scripts/play.ps1`.
  Lancé directement, `victory.exe` prend le chemin lent, et la documentation
  du module `host.rs` dit encore « every submission is waited for ».
- Beaucoup d'interrupteurs ne servent qu'à une expérience déjà tranchée. Ils
  restent dans le code chaud et dans les combinaisons à tester.

### 4.6 Structure du code de `victory-gpu`

- **Fichiers géants.** `host.rs` (6 701 lignes) mêle appareil, listes,
  soumission, relais, présentation et statistiques ; `resource.rs` 3 837,
  `shadow.rs` 3 186, `descriptor/host.rs` 3 070, `draw.rs` 2 760. Côté
  traducteur, `emit/lower.rs` fait 11 618 lignes et 211 fonctions.
- **État global.** 386 `static` dans `victory-gpu`, dont beaucoup sont des
  tables sous `Mutex` ou des compteurs atomiques partagés entre threads (par
  exemple la table des plages par tas dans `heap_buffers`). Les dépendances
  sont cachées, et ces parties ne se testent qu'avec un appareil réel.
- **Diagnostics mêlés au chemin chaud.** 191 blocs
  `cfg(feature = "diagnostics")`, des compteurs dans presque chaque fonction,
  et des lignes de journal en prose de plusieurs centaines de caractères.
  Pour les dépouiller, il faut des expressions régulières fragiles.
- **Mesures figées dans les commentaires.** Les chiffres cités (« run 52 »,
  « 2026-11-24 ») vieillissent sans que rien ne le signale.

### 4.7 Documentation

- Le code renvoie aux « decision 0003 » et « decision 0007 » (25
  occurrences), mais ces documents ne sont pas dans le dépôt.
- `docs/architecture.md` décrit le chargeur et le traducteur, mais pas le
  runtime GPU ni son modèle mémoire (copies, surveillance, relais, copies sur
  la carte, mémoire en place). C'est pourtant la partie la plus complexe et
  la plus coûteuse.
- Le tableau d'état du `README.md` cite une RTX 3050 et 1-3 fps. Les mesures
  récentes sont sur RTX 5070 à ~6 fps.

### 4.8 Mesure et boucle de développement

- **Pas de banc versionné.** Les mesures passent par des scripts écrits à la
  main dans des dossiers temporaires (`measure.ps1`, `analyze.py`), sans
  format commun ; chaque session les refait.
- **Le cache de shaders est indexé par le hash de l'exécutable.** Toute
  recompilation, même d'une ligne de journal, retraduit des milliers de
  shaders, et rate aussi le cache de pipelines du pilote quand le DXIL change. La première partie après un build
  n'est pas comparable (des pipelines encore « being made » 7 min plus tard
  quand le DXIL change).
- **Forte variance d'une partie à l'autre.** À nombre de draws égal, trois
  parties identiques ont donné 5,4-5,9, 5,2-6,0 et 6,2-6,4 fps. Un seul
  chiffre « fps moyen » ne permet pas de trancher : il faut comparer par
  fenêtres de charge égale et répéter.
- **Parties longues.** 7 min par mesure, fenêtre ouverte sur le bureau, et
  des fenêtres qui se ferment parfois en cours de route (« window closed »,
  cause inconnue : 2 de mes 10 parties, et 2 sur 4 lors d'une session
  précédente).

### 4.9 Qualité de code

- `rustfmt.toml` existe, mais le dépôt n'est pas formaté (beaucoup de fichiers
  diffèrent de `cargo fmt`), et la CI ne le vérifie pas.
- Plusieurs lints Clippy sont autorisés « until the newer code is tidied »
  (`type_complexity`, `needless_range_loop`…).
- La CI tourne sans carte graphique : les tests GPU y sont sautés ou passent
  par WARP. Les régressions de performance ne sont détectées nulle part.

## 5. Axes d'amélioration, par priorité

Gains et efforts sont des estimations ; les ordres de grandeur viennent des
mesures de la section 4.

### A. Un banc de mesure fiable (d'abord : effort faible, il conditionne tout le reste)

1. Versionner un script de mesure (`xtask bench` ou `scripts/measure.ps1`) :
   scénario fixe (touches, durée), sauvegarde mise de côté puis remise, sortie
   **CSV** par période (fps, ms, draws, préparation, attente carte, recopie,
   mémoire carte).
2. Indexer le cache de traductions par la version du **traducteur** (hash des
   sources de `victory-shader` au build) plutôt que par l'exécutable : un
   build qui ne touche pas au traducteur garde ses shaders et ses pipelines.
3. Comparer par fenêtres de charge égale (draws par frame), sur au moins deux
   parties.
4. Un mode court (90 s après le début du monde) pour les itérations, la partie
   de 7 min pour valider.

### B. Un défaut rapide et une configuration centralisée (effort faible)

1. Mettre dans le code les valeurs par défaut de `play.ps1` (relais, copies
   sur la carte…) ; garder les variables comme interrupteurs d'arrêt.
2. Une structure de configuration unique, lue au démarrage et journalisée une
   fois (valeurs effectives).
3. Supprimer les interrupteurs des expériences tranchées.

### C. Le modèle mémoire (le gros gain : effort élevé)

Objectif : que la carte lise et écrive la mémoire du titre sans copie
systématique ni rapatriement synchrone.

1. **Mémoire en place pour la mémoire privée du titre** (`inplace.rs`,
   niveaux 1 et 2). C'est la seule voie qui supprime à la fois la copie
   d'entrée et la recopie de sortie. Il faut mesurer ce que coûte la lecture
   à travers PCIe pour les gros buffers, et lever la limite `SEC_RESERVE` du
   pool de pages (`xmem`) pour ouvrir les sections.
2. **Écritures des shaders suivies sur la carte** : un masque de pages sales
   écrit par les shaders traduits, sur le même principe que les marques de
   lecture des textures de cette branche, à la place des comparaisons et des
   `fetch`. Cela vise les 105 ms d'attente du thread de recopie.
3. **Recopie sans attente** : rapatrier de façon asynchrone et ne bloquer que
   le lecteur qui en a besoin (le relais le fait déjà en partie).
4. **Rafraîchissements en parallèle** (`VICTORY_WIDE_REFRESH` existe, éteint) :
   à mesurer après 1 et 2.

### D. La soumission (effort moyen)

1. Réduire le temps noyau : regrouper les interrogations du write-watch,
   réduire les fautes de garde ; ce sont 57,6 ms par frame.
2. Réduire la section sous verrou (20,3 ms d'attente).
3. Une fois C fait, revoir le plafond de 128 listes en vol (35,5 ms
   d'attente).

### E. L'enregistrement (effort moyen ; prioritaire une fois C et D faits)

1. Mettre en cache, par draw, les liaisons qui ne changent pas : tables
   identiques, vues déjà en miroir, plages déjà à jour dans la liste.
2. Viser ~1 µs par appel de liste (contre ~5,7 µs aujourd'hui) : 41 000 appels
   par frame tiennent alors dans 33 ms sur 6 threads.

### F. Le GPU (après C, sinon les gains sont masqués)

1. Les passes de hachage et de comparaison disparaissent avec C.
2. Mesurer le coût des 2 506 bornages de commandes indirectes et regrouper ce
   qui peut l'être.
3. Lancer de rayons : piste séparée (image), avec son propre budget GPU.

### G. Structure du code (en continu)

1. Découper `host.rs` (appareil, listes, soumission, relais, présentation,
   statistiques) et `lower.rs` (par famille d'instructions).
2. Ramener l'état global dans des objets possédés (appareil, tas, liste), pour
   tester sans carte.
3. Un module de métriques unique, avec sortie structurée, à la place des
   compteurs dispersés.
4. Ajouter au dépôt les décisions 0003 et 0007, et un chapitre « runtime GPU
   et mémoire » dans `docs/architecture.md`.
5. Formater le dépôt une fois (`cargo fmt`), puis vérifier le format en CI.

## 6. Feuille de route proposée

| Étape | Contenu | Résultat attendu |
|---|---|---|
| 1 | A + B | mesures fiables en 2-3 min, défaut rapide |
| 2 | C.2 + C.3 (écritures des shaders suivies sur la carte, recopie asynchrone) | thread de recopie bien sous 33 ms |
| 3 | C.1 (mémoire en place) + D | thread de soumission bien sous 33 ms |
| 4 | E | enregistrement sous 33 ms par thread |
| 5 | F + lancer de rayons | budget GPU tenu avec l'éclairage complet |

Chaque étape se mesure avec le banc de l'étape 1 avant de passer à la
suivante. Pour l'instant, rien ne permet d'annoncer un fps final : les étapes
2 et 3 visent les deux goulots mesurés, et c'est seulement après elles qu'on
verra lequel prend la relève (enregistrement, temps propre du titre, GPU).

## 7. Points à éclaircir

- « after a present 35,8 ms until the first list call » : est-ce le temps de
  simulation du titre entre deux frames, ou une attente de notre fait ? À 30
  fps, il ne doit pas s'ajouter en série au reste.
- Les fermetures de fenêtre en cours de partie (voir 4.8) : cause inconnue.
- Les programmes abandonnés chaque frame (4 compute wave64 refusés par l'hôte,
  et `f133e35efed2a48a` qui ne se traduit pas) : effet visuel, et coût GPU à
  prévoir quand ils tourneront.
