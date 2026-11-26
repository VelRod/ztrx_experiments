# Plan d'action : 30 fps sur RTX 5060 (8 Go)

Date : 2026-11-26. Base : `docs/rapport.md` (mesures sur RTX 5070 12 Go,
Ryzen 7 5800X3D, profil Series X) et le code de `main` à `f29a555`. Le
profil Series S (`--console series-s`) est écarté : il ne fonctionne pas sur
les cartes NVIDIA.

## 0. Ce que les chiffres permettent de dire

- Le rapport mesure **5,5 à 6 fps sur une RTX 5070 12 Go** (179 ms par
  frame). Pour 30 fps il faut 33 ms : une frame 5 à 6 fois plus rapide.
- La RTX 5060 est nettement plus faible : 3 840 cœurs contre 6 144,
  448 Go/s de bande passante mémoire contre 672, 8 Go contre 12. Elle est
  reliée en **PCIe x8** au lieu de x16 ; sur une carte mère PCIe 4.0 (cas du
  5800X3D), la bande passante vers la carte est **deux fois plus faible**.
  Or l'architecture actuelle fait passer beaucoup de données par ce bus.
- **Risque GPU.** Sur la 5070, les commandes les plus lourdes du jeu coûtent
  déjà **au moins 40 ms par frame** (rapport, 4.4), plus que les 33 ms
  visés. Si c'est du vrai temps de calcul, la 5060 ne tiendra pas 30 fps
  même avec tout le travail CPU terminé. Le profil Series S étant exclu, et
  aucun interrupteur de résolution n'existant dans le dépôt, il faudra
  trouver ailleurs comment alléger la charge GPU (phase 5).
- **Risque VRAM, vérifié dans le code :**
  - les copies de textures prennent toujours jusqu'à 2 Gio, puis tant que
    la carte reste sous 90 % de son budget (`resource.rs`, `room`) ;
  - les copies de buffers en mémoire carte s'arrêtent à 80 % du budget
    (`shadow.rs`, `card_for`) ; `play.ps1` leur alloue 1 024 Mo ;
  - le code note qu'un dépassement de budget a terminé les runs 20 et 24 ;
  - le rapport ne donne pas le pic de VRAM sur la 5070 : on ne sait pas
    encore si 8 Go suffisent.

## Phase 0 : état des lieux sur la 5060 (1 à 2 jours)

1. Lancer `scripts/play.ps1` sans rien changer, en fermant ce qui occupe la
   VRAM (navigateur, etc.).
2. Dans le journal (`%TEMP%\ztrx.log`), relever la ligne
   `memory of textures: … the card: X MB in use of a budget of Y MB, Z MB of
   the PC's memory shared with it in use` :
   - si `X` reste collé à `Y`, ou si `Z` grimpe en cours de partie, la VRAM
     manque ;
   - si `passes that made none for lack of room` dépasse 0, des textures
     n'ont pas eu de place.
3. Comparer `VICTORY_CARD_BUFFERS_MB` à 1024 puis 768. Sur la 5070, 512 avait
   fait tomber le jeu de 6,5 à 2,9 fps : ne pas descendre à l'aveugle.
4. Répondre à la question 7 du rapport : les **35,8 ms entre un present et
   le premier appel de liste**. Si c'est le temps de simulation du jeu et
   qu'il s'additionne au reste, il dépasse à lui seul les 33 ms. Il faut le
   savoir avant tout gros chantier.
5. Mesurer le vrai temps GPU du jeu sur la 5060 (timestamps), pour savoir
   tout de suite si le risque GPU ci-dessus est réel.

## Phase 1 : banc de mesure et défauts rapides (rapport A + B, effort faible)

1. Versionner un script de mesure : scénario fixe, mode court de 90 s,
   sortie **CSV**. Ajouter des colonnes VRAM (utilisée, budget, mémoire PC
   partagée) et le nombre de draws par frame.
2. Indexer le cache de shaders par le hash des sources de `victory-shader`,
   pas par l'exécutable : sinon chaque build fausse la première partie.
3. Mettre dans le code les 7 réglages de `play.ps1` comme valeurs par
   défaut, et journaliser une fois au démarrage la configuration effective.
4. Comparer à charge égale (même nombre de draws), sur au moins deux
   parties : la variance mesurée va de 5,2 à 6,4 fps.

**Critère de sortie :** une mesure reproductible en 2 à 3 minutes, avec la
VRAM dans le CSV.

## Phase 2 : le thread de recopie (rapport C.2 + C.3)

- Les shaders traduits marquent eux-mêmes les pages qu'ils écrivent, à la
  place des comparaisons et des `fetch` (17 648 morceaux comparés par
  frame).
- La recopie devient asynchrone : on ne bloque que le lecteur qui a besoin
  des données.
- **Cible :** thread de recopie nettement sous 33 ms (147 ms aujourd'hui,
  dont 105 ms d'attente de la carte).
- **Pour la 5060 :** les 78,7 Mo rapatriés par frame passent par le lien
  x8. Réduire ce volume y rapporte plus que sur la 5070.

## Phase 3 : mémoire en place et soumission (rapport C.1 + D)

1. Avant tout code, lancer la sonde `VICTORY_INPLACE_PROBE=1` **sur la
   5060**. La mémoire en place fait lire la mémoire du jeu à la carte à
   travers PCIe ; sur un lien x8, ce coût peut changer la conclusion.
2. Pour passer `VICTORY_INPLACE` à 2, le code exige de ne pas l'associer à
   `VICTORY_LATE_WAIT` (`inplace.rs`, `level`). L'interaction de cette règle
   avec le relais (`VICTORY_RELAY=2`) n'est pas vérifiée : à contrôler avant
   de les combiner.
3. Lever la limite `SEC_RESERVE` du pool de pages (`xmem`).
4. Soumission :
   - réduire le temps noyau (57,6 ms : write-watch, fautes de garde) ;
   - réduire l'attente du verrou (20,3 ms) ;
   - revoir ensuite le plafond de 128 listes en vol (35,5 ms d'attente).
5. **Atout sur 8 Go :** la mémoire en place n'a plus besoin des copies en
   mémoire carte ; les textures récupèrent cette VRAM.

**Cible :** thread de soumission sous 33 ms.

## Phase 4 : l'enregistrement des listes (rapport E)

Passer d'environ 5,7 µs à environ 1 µs par appel, en mettant en cache les
liaisons inchangées. Les 41 000 appels par frame tiennent alors dans 33 ms
par thread.

## Phase 5 : le GPU (rapport F)

1. Après la phase 3, les 359 passes de hachage et de comparaison
   disparaissent. Mesurer ensuite les 2 506 bornages de commandes
   indirectes et regrouper ce qui peut l'être.
2. Si le temps GPU du jeu seul dépasse encore 33 ms sur la 5060 : regarder
   la qualité du code généré par `victory-shader` sur les dispatchs et draws
   indirects les plus lourds (20,5 ms et 15,7 ms sur la 5070), et chercher
   un moyen de baisser la résolution de rendu que le jeu choisit. Aucun
   interrupteur de ce type n'existe dans le dépôt aujourd'hui.
3. Le lancer de rayons (`VICTORY_BVH_BUILDER`) ajoutera environ 350
   dispatchs par frame, sur une carte qui a moins de cœurs RT. Il passe en
   dernier, avec son propre budget, et reste coupé s'il ne tient pas.

## Règle de conduite

Chaque phase se mesure avec le banc de la phase 1 avant de passer à la
suivante. Comme le dit le rapport, on ne verra qu'après les phases 2 et 3
quel goulot prend la relève. Aujourd'hui, rien ne garantit 30 fps sur une
5060 : le point décisif est le temps GPU propre au jeu, à mesurer dès la
phase 0.
