# myMoney

Gestionnaire de patrimoine personnel **auto-hébergé** (NAS, Docker) : comptes bancaires, livrets, assurance-vie, bourse, crypto, immobilier. Tout est **chiffré au repos** et **aucune banque n'est connectée** : tu déposes tes relevés (PDF, CSV, OFX…), des **adapters** les lisent, et l'application en tire les comptes, leurs titulaires, leurs RIB et leurs opérations.

## Fonctionnalités

- **Dépose un fichier, le bon adapter est choisi tout seul.** Chaque adapter lit un type de document d'un établissement ; un établissement peut en avoir plusieurs (relevé mensuel PDF, export CSV, relevé d'épargne…). En cas de doute (deux lectures plausibles, établissement reconnu mais document inconnu), l'application te demande laquelle utiliser.
- **Copier ses coordonnées en un clic** : sur la fiche d'un compte, un bouton « Copier » à côté du titulaire, de l'adresse, de l'IBAN (copié sans espaces), du BIC et du RIB, et « Tout copier » pour envoyer titulaire + IBAN + BIC + RIB d'un coup. Même chose pour l'IBAN des contacts. (Sans HTTPS, le navigateur interdit l'accès direct au presse-papiers : l'application passe alors par la méthode de secours, qui marche aussi.)
- **Masquer les données sensibles** : le bouton « Masquer » de la barre du haut floute les montants, les IBAN et RIB, les noms, adresses, libellés d'opérations, noms de fichiers et les graphiques (infobulles comprises), et empêche de les sélectionner. Le mode reste actif sur l'appareil jusqu'à « Afficher ».
- **60 styles** (bouton « Style ») : 30 clairs (dont Clair, Neige, Sable, Menthe, Lavande, Ciel, Rose poudré, Papier, Solarized clair, Agrume, Aube, Brume, Coton, Perle, Lin, Amande, Pistache, Citron, Abricot, Corail, Pêche, Framboise, Lilas, Glycine, Océan, Lagune, Givre, Argile, Nuage, Pivoine) et 30 sombres (dont Sombre, Minuit, Ardoise, Charbon, Forêt, Aubergine, Nord, Dracula, Solarized sombre, Noir OLED, Cosmos, Orage, Encre, Obsidienne, Sapin, Bordeaux, Prune, Cobalt, Abysse, Braise, Cuivre, Mousse, Crépuscule, Aurore, Fusain, Ébène, Pétrole, Néon, Cerise, Brouillard), plus « Automatique » qui suit l'appareil. Le contraste de chaque style est vérifié par les tests (texte, liens, montants, graphiques). Le choix est gardé sur l'appareil. Les couleurs sont définies dans `src/web/themes.rs`.
- **Deux looks** (bouton ✨ de la barre) : le look classique (barre en haut) et le look moderne (panneau latéral flottant sur ordinateur, onglets en bas façon application sur téléphone, cartes arrondies, boutons en pastille, bandeau en dégradé). Il se combine avec les 60 styles et le choix est gardé sur l'appareil (`web/static/modern.css`).
- **Documents classés comme un dossier** : établissement → **compte** → type de document (relevé de compte, récapitulatif des frais, offre de prêt…) → année → fichier. On clique pour déplier, on coche les fichiers voulus (ou tout un groupe) et « Télécharger la sélection » donne le fichier, ou un zip s'il y en a plusieurs. Chaque ligne affiche à gauche « Type de fichier - date » (lien bleu qui ouvre le document), et à côté les noms qui y figurent, le nom du fichier, son état et la fiabilité.
- **Fichiers importés renommés** : à l'import, chaque fichier prend un nom standard, sans accents, en majuscules : `ETABLISSEMENT_TYPE-DE-COMPTE_TYPE-DE-FICHIER_NOM_PRENOM_AAAA-MM-JJ.extension` (les mots et la date sont séparés par des `-`, les champs par des `_`). Si le nom dépasse la taille maximale d'un nom de fichier, les prénoms sont retirés, puis tous les noms. Deux fichiers au même nom reçoivent `_2`, `_3`… Le nom d'origine est gardé (visible sur la page du document) et revient si l'import est annulé. Un changement des titulaires du compte renomme ses documents.
- **Titulaires en plusieurs cases** : sur un compte (et dans le formulaire d'import), une civilité (M, MME, MLLE), plusieurs cases de nom, de nom de naissance (si différent) et de prénom, et autant de personnes que nécessaire pour un compte joint, pour remplir les formulaires qui demandent un seul nom ou les deux. Les cases sont préremplies d'après le nom imprimé sur le document. La fiche du compte a un bouton « Copier » pour chaque champ.
- **« Actualiser à la dernière version »** : quand la lecture s'améliore, la page Documents repère les fichiers (validés ou non) lus par une version plus ancienne et propose un bouton pour les repasser tous dans la lecture actuelle (renommage et titulaires compris), ou un par un depuis la page du document.
- **Où part l'argent** (Analyses → Dépenses et revenus) : chaque opération reçoit une catégorie (Courses, Restaurants, Transports, Énergie et eau, Abonnements, Santé, Impôts, Crédit, Frais bancaires…) deviné d'après le libellé et le type d'opération, sans rien envoyer à personne. On voit le total par catégorie (anneau et barres) et mois par mois, puis, en cliquant une catégorie, **chez qui** (par bénéficiaire) et **quand** (chaque opération). Si une catégorie est fausse, tu la changes pour un bénéficiaire : c'est retenu pour toutes ses opérations, y compris les prochaines (réglage personnel de chaque utilisateur ; « Automatique » le retire). « Autres dépenses » regroupe ce qui n'a pas été reconnu. Les revenus ont leur propre anneau sur la même page, et un clic sur un mois ouvre le détail de ce mois.
- **« Qui est-ce ? » sans doublon** : dans la liste « À identifier », on choisit un contact déjà enregistré (la même personne avec un autre compte ou un autre nom imprimé) au lieu d'en créer un nouveau ; son nom imprimé et son IBAN sont ajoutés à ce contact.
- **Tout est rendu cohérent et contrôlé** :
  - le solde initial + les opérations doivent donner le solde final du relevé ;
  - le relevé doit prolonger le précédent (soldes qui se suivent, pas de mois manquant) ;
  - les **doublons sont signalés** : même fichier déposé deux fois, même relevé retéléchargé, relevés qui se chevauchent, opérations « probablement identiques » venues d'une autre source. Rien n'est ajouté deux fois, et tu vois ce qui a été ignoré ;
  - **déduplicage des fichiers** : à chaque dépôt, l'application regarde si le relevé (même compte, même période) est déjà importé et compare les opérations et les soldes.
    - *Aucune différence* : le fichier est dit « doublon », **écarté** (supprimé automatiquement au bout de 24 h) ; tu peux le garder en copie ou le supprimer tout de suite. Un fichier strictement identique (même contenu octet pour octet) n'est jamais ajouté deux fois.
    - *Des différences* (opérations en plus, opérations absentes, soldes autres) : elles sont listées et tu choisis **remplacer l'ancien** (l'ancien fichier et les opérations qu'il avait apportées sont retirés, le nouveau est importé en entier), **stocker avec l'ancien** (les deux fichiers restent, seules les opérations nouvelles sont ajoutées) ou **écarter** le nouveau ;
  - **relevés manquants** : si les relevés d'un compte couvrent janvier à avril puis juin à décembre, l'application détecte qu'il manque mai. Le trou est signalé **sur le tableau de bord** et sur la page du compte, avec deux actions : **déposer le relevé manquant** (le trou disparaît dès qu'il est importé) ou **« c'est normal »** (compte sans mouvement, relevé jamais émis…). Une marque « normal » couvre aussi les morceaux du trou si un relevé partiel le découpe ensuite, mais un autre trou, ou un trou plus grand, est signalé à nouveau ; elle se retire depuis la page du compte (« Remettre en alerte ») ;
  - un import peut être **annulé** (les opérations qu'il a apportées sont retirées).
- **Identité des comptes** lue dans les documents : titulaire, adresse, IBAN, BIC, RIB (banque, guichet, compte, clé), vérifiés (clé RIB, IBAN).
- **Contacts** : à qui va l'argent, d'où il vient. Tu nommes une fois un IBAN ou un nom imprimé par la banque (avec un téléphone, des notes…) ; l'application le retient et le reconnaît dans tous les imports suivants, quelle que soit l'orthographe (« DUPONT Jean », « M Jean Dupont »). Les virements entre tes propres comptes sont repérés (par ton nom, et en appariant un montant opposé le même jour sur deux de tes comptes) : ils ne comptent ni comme revenu ni comme dépense dans les analyses.
- **Comptes joints** : la banque coupe le nom du titulaire (« M DUPONT OU MME LEROY CLAIR… ») ; l'application te demande les noms complets une seule fois (sur n'importe quel document : relevé, récapitulatif des frais, offre de prêt, dès qu'un nom contient OU, ET, une seconde civilité ou plus de mots qu'un nom et un prénom), les garde, et reconnaît quand même le nom coupé dans les relevés de tes autres comptes.
- **Invalidation protégée** : tous les boutons « Invalider » (un document, une sélection, un compte, un établissement, tout) et « Annuler tous les imports et tout relire » sont tout rouges et demandent **trois clics de suite** (le bouton compte à rebours, et repart de zéro après 5 secondes sans clic).
- **Documents originaux conservés chiffrés** (illisibles sur le disque) et **téléchargeables** depuis l'application.
- **Analyses** (onglet dédié, graphiques SVG générés sur le NAS, sans bibliothèque externe) :
  - *Évolution* : courbe du patrimoine, au total, par compte ou par catégorie (c'est aussi la répartition : le tableau des comptes donne la valeur et la part à la date choisie). L'historique de chaque compte est reconstitué à partir du solde des relevés et des opérations importées, et celui d'un prêt à partir de son échéancier (la dette ne « tombe » plus d'un coup à la fin). Case « Masquer les dettes » quand un prêt écrase les autres lignes ;
  - *Dépenses et revenus*, sur une même page : le solde de la période, les mois côte à côte (barres), puis deux anneaux (dépenses, revenus) avec leurs catégories et leurs plus gros bénéficiaires ou sources ; un clic sur un mois ou sur une catégorie détaille, avec ou sans les virements entre tes propres comptes ;
  - **période** : 1, 3, 6 mois, 1 an, tout, ou **deux dates au calendrier** (Du… au…) ; la date de fin sert aussi à valoriser les comptes « à cette date » ;
  - choix des comptes pris en compte (la somme totale, les actifs, les dettes et la variation suivent la sélection) et tableau des comptes triable par colonne, avec total. La variation ne compte que les comptes déjà suivis au début de la période : ajouter un compte n'est pas de la croissance.
- **Comptes** (onglet dédié) : tous tes comptes en cartes (titulaires, IBAN avec bouton copier, solde, nombre de documents, archivés à part), « Modifier » pour la fiche complète, « Renommer » pour changer le nom, l'établissement et le type sur place (les fichiers du compte sont renommés).
- **Comptes joints** : dès qu'un compte a plus d'une personne, il est marqué « Joint » à côté de son type (« Compte courant - Joint ») partout où le type s'affiche, avec le nom des titulaires à côté ; la page Comptes se filtre sur Tous, Comptes joints ou Individuels. Le marquage se déduit des titulaires : il n'y a rien à cocher.
- **Personnes** (onglet à côté de Comptes) : une fiche par personne (civilité, noms, noms de naissance, prénoms, notes), liée à un ou plusieurs comptes, joints ou non. Dans les formulaires de compte et d'import, on choisit une personne déjà enregistrée au lieu de retaper ses noms ; un nom imprimé reconnu (« M DUPONT JEAN OU MME LEROY CLAIRE ») est proposé avec ses fiches. Une fiche créée en tapant des noms est réutilisée si les mêmes noms reviennent. La page d'une personne liste ses comptes avec leur total et ouvre les analyses sur ces seuls comptes ; modifier la fiche met à jour tous ses comptes et renomme leurs fichiers. Les comptes d'avant les fiches reçoivent les leurs en ouvrant la page Personnes. Supprimer une fiche garde les noms sur les comptes, sans lien.
- **Accueil** : le patrimoine net et sa courbe (mêmes axes et mêmes infobulles que les analyses, avec le choix de la période).
- **Patrimoine net** avec répartition (liquidités, épargne, investissements, crypto, immobilier, dettes) et historique.
- **Taux automatiques et taux demandés** :
  - Livret A, LDDS, LEP, CEL : l'application lit leur taux sur les pages officielles de service-public.gouv.fr **à chaque démarrage, puis toutes les 24 heures** (une heure après un échec). Seules ces pages publiques sont demandées, rien de personnel n'est envoyé ; la dernière lecture est gardée dans `public_rates.json` du dossier de données, et si le réseau manque la table intégrée (`src/services/rates.rs`) prend le relais. Le Livret Jeune suit le Livret A (minimum légal). Paramètres → Taux réglementés montre la dernière lecture et un bouton « Actualiser maintenant ».
  - Livret de ta banque, PEL (son taux dépend de la date d'ouverture), compte à terme, assurance-vie, crowdlending : personne ne publie ce taux, l'accueil affiche donc **« Taux à renseigner »** avec un champ par compte (ou « Sans intérêt »), et la fiche du compte permet de le modifier. On peut aussi le donner dès l'import d'un nouveau compte. Un taux saisi gagne toujours sur les taux lus.
  - **Brut ou net** : à côté de chaque taux saisi, une case Brut / Net. La fiche du compte affiche les deux (« 3,00 % brut · 2,06 % net ») d'après le **tableau fiscal** (ci-dessous). Livret A, LDDS, LEP et Livret Jeune ne sont pas imposés (brut = net) ; le CEL ne paie que les prélèvements sociaux ; l'assurance-vie prend sa ligne « rachat avant 8 ans » ; le reste paie la flat tax (cas particuliers, comme un PEL ancien : le net est indicatif).
- **Fiscalité** (Paramètres → Ouvrir le tableau fiscal, administrateur) : un tableau de taux par catégorie (intérêts et flat tax, dividendes, plus-values, crypto, PEA, PER, assurance-vie avant/après 8 ans, revenus fonciers, plus-values immobilières), **prérempli avec les chiffres de 2026** (31,4 % de flat tax : 12,8 % d'impôt + 18,6 % de prélèvements sociaux) d'après [LégiFiscal](https://www.legifiscal.fr/reperes-fiscaux/flat-tax-pfu-champ-application-taux-applicables.html), dont le lien est dans l'application. Chaque chiffre est modifiable, avec « Rétablir 2026 ». Comme ces taux changent avec les lois de finances, **l'administrateur voit une fenêtre de rappel une fois par mois** (« Ils sont à jour » / « Vérifier et mettre à jour » / « Me le redire demain ») ; elle signale aussi quand impots.gouv.fr, lu avec les taux des livrets, indique une flat tax différente du tableau. Les lignes ajoutées par une version future apparaissent toutes seules, et ce tableau servira plus tard à un calculateur d'impôt.

- **Comptes manuels** : livret, bien immobilier, crédit, positions crypto ou bourse saisies à la main, valorisées avec des prix gratuits (CoinGecko, Yahoo Finance, BCE via Frankfurter).

### Documents pris en charge

| Établissement | Adapter | Document |
|---|---|---|
| BoursoBank | `boursobank.statement_pdf` | PDF « Extrait de votre compte en EUR » : compte courant, compte joint, LDDS et autres livrets (même mise en page, avec RIB et IBAN ; le type du compte est lu sur la ligne « Compte d'épargne : … ») |
| BoursoBank | `boursobank.savings_pdf` | PDF « Extrait de votre Plan Epargne Logement » (épargne : numéro de compte, sans RIB ni IBAN ; validé sur un PEL) |
| Fortuneo | `fortuneo.statement_pdf` | PDF « Relevé de Compte » ou « Relevé Livret » mensuel (compte courant, compte joint, Livret +…) : RIB, IBAN, BIC, titulaires, soldes, opérations (virements, chèques, intérêts) ; contrôlé aussi avec les totaux débit / crédit imprimés par la banque. Un livret n'a pas d'IBAN : il est reconnu par son numéro. Premier relevé (sans date d'ancien solde) accepté. Sur un compte joint, le nom des titulaires, coupé par la banque à 30 caractères, est recollé puis **demandé une seule fois** pour confirmation |
| Fortuneo | `fortuneo.fees_pdf` | PDF « Relevé de frais » annuel : frais et intérêts de l'année, service par service. Aucune opération : il est **rattaché au compte** (retrouvé par l'IBAN) et comparé aux lignes de frais des relevés importés |
| Fortuneo | `fortuneo.deposit_guarantee_pdf` | lettre annuelle sur la garantie des dépôts (FGDR) : aucune opération, elle est **classée automatiquement** avec tes papiers (chiffrée, téléchargeable) |
| Fortuneo | `fortuneo.life_arbitrage_pdf` | **confirmation d'arbitrage** de ton assurance-vie Fortuneo Vie (lettre Suravenir) : contrat, date, montant, fonds vendus et achetés (le total acheté est contrôlé). Aucune opération : classé avec tes papiers, rubrique « Assurance-vie » |
| Fortuneo | `fortuneo.loan_offer_pdf` | **offre de prêt immobilier** : montant, durée, taux, TAEG, coût du crédit et tableau d'amortissement d'origine (numéroté) |
| Fortuneo | `fortuneo.loan_prepayment_pdf` | courrier de **remboursement anticipé partiel** : capital restant dû, échéance et date de fin avant / après |
| Fortuneo | `fortuneo.loan_schedule_pdf` | **tableau d'amortissement daté** de l'espace client (échéance par échéance, du jour à la fin) |
| Revolut | `revolut.statement_csv` | export CSV d'un compte |
| tous | `generic.ofx`, `generic.qif` | fichiers OFX / QFX et QIF |
| tous | `generic.csv`, `generic.pdf` | lecture « devinée » (colonnes date / montant / débit / crédit) : jamais importée sans que tu voies ce qui a été compris |

**Un prêt se reconstitue à partir de ses documents**, déposés dans n'importe quel ordre : le contrat, un remboursement anticipé, le tableau daté. L'application les rejoue (le contrat jusqu'au remboursement, un pont calculé jusqu'au tableau daté, puis le tableau) et contrôle chaque jonction : le capital restant dû avant le remboursement doit être celui que le contrat donne à ce moment-là, le plan recalculé doit retomber sur le tableau daté, les dates de fin doivent concorder, les intérêts doivent se recalculer avec le taux, les prélèvements de tes relevés doivent correspondre aux échéances, et le capital doit finir à zéro. La fiche du prêt (compte de type « Crédit immobilier ») montre le taux, les dates, le capital restant dû aujourd'hui, la prochaine échéance, ce qu'il reste à payer (intérêts et assurance), ce que le remboursement anticipé fait économiser, et toutes les échéances ; son solde alimente le patrimoine (dettes) et se met à jour chaque mois. S'il manque un document (par exemple le courrier d'un remboursement anticipé), l'écart est signalé.

Certains documents ne contiennent aucune opération (récapitulatif des frais, lettres d'information) : ils sont reconnus, datés et conservés comme les autres fichiers, et leur contenu utile (frais, plafond de garantie) est gardé. Dans la bibliothèque, chaque document affiche son type et son établissement.

Pour ajouter un adapter (un autre établissement, ou un autre type de document d'un établissement déjà géré), il suffit d'un document d'exemple : voir « Développement ».

## Sécurité

Résumé (détails dans [SECURITY.md](SECURITY.md)). Les relevés contiennent des données très personnelles (nom, adresse, RIB, bénéficiaires) : elles ne sont jamais écrites en clair sur le disque, et **le dépôt Git n'en contient aucune** (les tests fabriquent des relevés fictifs ; `.gitignore` écarte les PDF, CSV et OFX).

- **Base de données chiffrée** (SQLCipher, AES-256) : comptes, titulaires, adresses, RIB, opérations, contacts. **Documents originaux chiffrés** un par un (AES-256-GCM, nom de fichier aléatoire).
- **Aucune connexion à une banque**, aucun identifiant bancaire stocké : l'application ne reçoit que ce que tu lui donnes.
- La clé maîtresse est aléatoire. Elle n'est stockée que **chiffrée** par ton mot de passe (Argon2id) et par un **code de secours** affiché une seule fois. **Rien n'est lisible sur le NAS quand le coffre est verrouillé** (au démarrage, sur demande ou après 15 min d'inactivité).
- Interface web durcie : CSP stricte sans aucune ressource externe, cookies HttpOnly et SameSite=Strict, jetons CSRF, anti-force brute, code d'installation à usage unique.
- Conteneur durci : utilisateur non root, système de fichiers en lecture seule, aucune capability, `no-new-privileges`.

> ⚠️ **Sans le mot de passe ni le code de secours, les données sont irrécupérables.** C'est voulu.

## Installation sur le NAS

Le plus simple : `docker-compose.nas.yml`. Rien à construire ni à préparer à la main (un petit conteneur d'init crée le dossier `data` avec les bons droits).

1. Copie `docker-compose.nas.yml` dans un dossier du NAS (ex. `/volume1/docker/mymoney/`) et adapte le chemin du dossier `data` si besoin.
2. Lance `docker compose -f docker-compose.nas.yml up -d` (Synology : Container Manager → Projet → Créer, colle le fichier).
3. Récupère le code d'installation : `docker logs mymoney`.
4. Ouvre `http://IP-DU-NAS:10458`, crée ton compte administrateur (e-mail et mot de passe) et **note le code de secours**.

L'image `ghcr.io/ferrandj/mymoney` est publiée par la CI GitHub (amd64 et arm64). Chaque push sur `main` dont les tests passent devient une release (`v1.0.0`, `v1.0.1`…) avec les tags `latest`, `1`, `1.0` et `1.0.1`. Pour monter de version mineure ou majeure, mets `[minor]` ou `[major]` dans la première ligne du message de commit, ou lance le workflow à la main. Si le `pull` est refusé, rends le paquet public (GitHub → Packages → mymoney → Package settings) ou fais un `docker login ghcr.io`.

Mise à jour : `docker compose -f docker-compose.nas.yml pull && docker compose -f docker-compose.nas.yml up -d` (ou Watchtower). Le dossier `data` n'est jamais touché.

Sans le fichier NAS : `docker-compose.yml` (même image, port 8000, dossier `./data` à créer avec `chown 10001:10001`), ou `docker compose build` après avoir décommenté `build: .`.

### Installer l'application sur Android

myMoney est une application web installable (PWA). Ouvre-la dans Chrome sur le téléphone, puis menu ⋮ → **Installer l'application** (ou le bouton dans Paramètres). Elle s'ajoute à l'écran d'accueil et s'ouvre en plein écran.

Le navigateur exige **HTTPS** pour l'installation (en `http://IP-DU-NAS:10458`, Chrome ne propose qu'un simple raccourci). Deux façons simples, sans ouvrir de port sur Internet :

- **Tailscale** : active HTTPS dans la console Tailscale, puis `tailscale serve --bg --https=443 http://127.0.0.1:10458` sur le NAS. L'adresse devient `https://<nas>.<tailnet>.ts.net`, accessible depuis le téléphone avec l'app Tailscale.
- **Reverse proxy Synology** : Panneau de configuration → Portail de connexion → Avancé → Proxy inversé, avec un certificat pour un nom de domaine local.

Ensuite, mets `MYMONEY_COOKIE_SECURE=true` (et `MYMONEY_TRUSTED_PROXIES` si un proxy est devant). L'application n'enregistre rien dans le navigateur : sans réseau elle affiche seulement « myMoney est injoignable ».

### Accès à distance

N'ouvre **pas** le port sur Internet. Utilise un VPN (Tailscale ou WireGuard, souvent disponibles sur le NAS) ou un tunnel Cloudflare avec Cloudflare Access (ci-dessous). Pour le HTTPS en local, place l'app derrière le reverse proxy du NAS et passe `MYMONEY_COOKIE_SECURE=true`.

## Plusieurs utilisateurs

- **Un utilisateur = une adresse e-mail + son propre mot de passe** vers le même coffre. Le premier compte (créé à l'installation) est administrateur ; il ajoute les autres dans **Paramètres → Utilisateurs** (e-mail, nom, mot de passe de départ), peut nommer d'autres administrateurs, redéfinir le mot de passe d'un utilisateur qui l'a oublié, ou supprimer un utilisateur (ses comptes passent à l'administrateur qui le supprime).
- **Chacun ne voit que ses comptes** et ceux qu'on lui partage : portefeuille, analyses, documents, opérations, trous de relevés. Un virement entre deux personnes n'est pas un « virement entre mes comptes ».
- **Partager un compte, un par un** : sur la page du compte, « Partage » → choisis la personne et les droits, *lecture seule* (voir solde, opérations, IBAN, documents) ou *lecture et modification* (déposer des relevés, corriger). Le propriétaire seul supprime le compte et gère le partage ; la personne peut quitter un partage. Un compte partagé compte dans le patrimoine de chacun.
- **Les contacts (bénéficiaires) forment un carnet commun** ; leurs statistiques ne comptent que les opérations que tu peux voir.
- Tout est protégé par **une seule clé de données**, enveloppée séparément par le mot de passe de chaque personne (comme les emplacements de clé d'un disque chiffré) : changer son mot de passe ne touche pas aux autres. La séparation entre utilisateurs est assurée par l'application, pas par le chiffrement : l'administrateur du NAS peut techniquement tout lire (voir [SECURITY.md](SECURITY.md)).
- **Mise à jour depuis la version à un seul mot de passe** : à la première connexion, entre ton mot de passe habituel ; l'application te demande ton e-mail et ton nom, tu deviens administrateur, tes comptes et documents te sont attribués, et ton mot de passe reste le même.

### Connexion automatique avec Cloudflare Access

Tu mets l'application derrière un tunnel Cloudflare (`cloudflared`) protégé par Cloudflare Access (liste d'e-mails autorisés). Cloudflare ajoute à chaque requête un **jeton signé** qui contient l'e-mail vérifié du visiteur : l'application le vérifie et connecte directement l'utilisateur correspondant, **sans mot de passe**. Un accès direct sur le réseau local (`http://adresse-du-nas:10458`) continue de demander e-mail et mot de passe.

1. **Tunnel** (Zero Trust → Networks → Tunnels) : un *Public hostname* `mymoney.ton-domaine.xyz` → service `HTTP` → `ADRESSE-DU-NAS:10458` (l'adresse IP du NAS et le port, séparés par `:`, sans `/` avant le port).
2. **Application Access** (Zero Trust → Access → Applications → *Self-hosted*) pour le même nom d'hôte, avec une règle **Allow** sur les e-mails (un par un, ou un domaine). Cloudflare gère la connexion (code par e-mail, Google…).
3. Dans cette application Access, note l'**Application Audience (AUD) Tag** (Overview) ; le **nom de ton équipe** est celui de `https://<équipe>.cloudflareaccess.com` (Zero Trust → Settings → Custom pages).
4. Donne-les à l'application (docker compose, voir `docker-compose.nas.yml`) :
   ```yaml
   MYMONEY_CF_TEAM_DOMAIN: "mon-equipe"          # ou mon-equipe.cloudflareaccess.com
   MYMONEY_CF_AUD: "<l'AUD tag de l'étape 3>"
   MYMONEY_TRUSTED_ORIGINS: "https://mymoney.ton-domaine.xyz"
   ```
   puis `docker compose up -d`. Dans **Paramètres → Cloudflare Access** l'application indique « Activé ».
5. **Crée chaque utilisateur** dans Paramètres → Utilisateurs avec **la même adresse e-mail** que dans Cloudflare Access. Une adresse que Cloudflare autorise mais qui n'a pas de compte voit « n'a pas de compte sur myMoney ».

À savoir :
- **Seul le jeton signé compte**, jamais l'en-tête `Cf-Access-Authenticated-User-Email` : l'application répond aussi sur ton réseau local, où n'importe qui pourrait envoyer un faux en-tête. Le jeton est vérifié (signature avec les clés publiques de Cloudflare, application visée, émetteur, expiration). Elle doit pouvoir joindre `https://<équipe>.cloudflareaccess.com` pour récupérer ces clés.
- **Le coffre doit être ouvert** pour que Cloudflare fasse entrer quelqu'un : la clé de données n'existe en mémoire qu'une fois un mot de passe saisi (ou le déverrouillage automatique). Après un redémarrage du NAS, la première personne entre son mot de passe (la page le propose, e-mail prérempli) ; ensuite tout le monde entre par Cloudflare. Pour que le coffre ne se referme pas tout seul au bout de 15 minutes d'inactivité, mets `MYMONEY_AUTO_LOCK_MINUTES: "0"` (jamais) : la clé reste alors en mémoire jusqu'au redémarrage. Le déverrouillage automatique au démarrage (`MYMONEY_UNLOCK_PASSWORD_FILE` + `MYMONEY_UNLOCK_EMAIL`) évite même cette saisie mais range le mot de passe sur le NAS (voir SECURITY.md).
- Le bouton **Verrouiller** (administrateur) ferme le coffre pour tout le monde ; les autres utilisateurs ont « Déconnexion ». Déconnecté via Cloudflare, tu es aussi déconnecté d'Access.
- Le cookie de session est marqué *Secure* automatiquement quand Cloudflare indique que le navigateur est en HTTPS (`X-Forwarded-Proto`), et pas en accès direct en `http` local : tu n'as pas besoin de choisir.

### Sauvegardes

Sauvegarde le dossier `data/` tel quel (Hyper Backup, rsync…). Il est chiffré : la sauvegarde l'est aussi. Garde le code de secours **ailleurs** que sur le NAS.

## Ajouter tes comptes

1. Télécharge le relevé dans l'espace client de ta banque (PDF, CSV ou OFX).
2. Va dans **Documents** et **dépose** le fichier (glisser-déposer, plusieurs fichiers possibles).
3. Le document est chiffré, reconnu, lu, puis contrôlé :
   - compte déjà connu (même IBAN / même RIB), relevé cohérent, sans doublon : **importé automatiquement** ;
   - un premier relevé (compte pas encore connu) reste **« à valider »** : la page Documents affiche un encadré « À valider » (avec, pour chaque document, la raison qui le retient). À côté de chaque document, une pastille **fiabilité** (élevée / moyenne / faible) dit si les contrôles ont réussi ; **Passer en revue un par un** ouvre les documents à valider l'un après l'autre ;
   - sinon, une page de vérification montre ce qui a été compris (titulaire, RIB, soldes, opérations, doublons, anomalies) et te laisse choisir le compte, l'adapter ou ce qu'il faut importer.
4. Dans **Contacts**, nomme les bénéficiaires et émetteurs qui reviennent : ils seront reconnus automatiquement ensuite.

Un livret, un bien immobilier, un crédit ou des positions crypto sans relevé : **+ Compte manuel** sur la page Patrimoine.

Un trou n'est signalé que s'il est vrai : une période couverte par un relevé déjà **lu mais pas encore validé** n'est pas « manquante », elle est en attente.

Chaque document garde l'empreinte de la version de lecture qui l'a lu (visible en bas de page : « myMoney 1.x.y · lecture abc123 »). Un document **en attente** lu par une version plus ancienne est relu automatiquement quand tu l'ouvres ou que tu ouvres la page Documents ; un document **déjà importé** n'est jamais modifié en silence : sa page indique « lu par une version plus ancienne » et **Tout relire** le met à jour.

Dans **Analyses → Dépenses**, un bloc **Frais bancaires** met côte à côte ce que la banque annonce dans ses récapitulatifs de frais (facturé, remboursé, payé, avec le détail des services) et les opérations de frais trouvées dans tes relevés sur la même période.

## Après une mise à jour : tout relire

Les lectures s'améliorent (nouveaux formats, corrections). Dans **Documents → « Tout relire depuis les fichiers »**, un bouton **annule tous les imports** puis **relit chaque fichier** avec les lectures actuelles. Tes fichiers ne sont jamais supprimés ; les comptes gardent leur nom, leurs titulaires et leurs partages, mais perdent les opérations, relevés, récapitulatifs et positions apportés par les documents (les opérations saisies à la main restent). Ce qui est évident est revalidé ; ce qui ne passe plus les contrôles (nouvel avertissement, solde qui ne s'enchaîne plus…) reste **à valider**, avec la raison affichée : rien n'est validé en silence. Sur la page d'un document déjà importé, **Invalider l'import** retire seulement ses opérations ; dans la bibliothèque, **Invalider** (par type de document ou par établissement), **Invalider la sélection** et **Tout invalider** font la même chose en groupe (les fichiers restent, ils reviennent dans « À valider »).

## Développement

L'application est écrite en **Rust** (un seul binaire, sans dépendance à l'exécution). La version Python d'origine est
gardée telle quelle dans [`python/`](python/), en secours et comme **référence** : la CI vérifie à chaque push que la
version Rust lit les mêmes documents et rend les mêmes gabarits, et que son application web répond sans régression
(les nouveautés propres à Rust étant tolérées).

```bash
cargo build --release
cargo test --release
MYMONEY_DATA_DIR=./data ./target/release/mymoney serve   # http://127.0.0.1:8000
```

Structure :

```
src/
  security/    crypto (AES-GCM, Argon2id, HKDF), trousseau de clés, coffre en mémoire
  db.rs        base SQLCipher ouverte uniquement quand le coffre est déverrouillé (même format que la version Python)
  pdf/         lecture des PDF comme pdfplumber (caractères, positions, mots, texte), PDF chiffrés compris
  adapters/    un adapter par type de document et par établissement
    base.rs        contrat (Adapter, SourceDocument, ParsedStatement, TransactionData)
    registry.rs    choix automatique de l'adapter (score de chaque adapter, question à l'utilisateur en cas de doute)
    boursobank/, fortuneo/, revolut/, generic/ (OFX, QIF, CSV et PDF « devinés »)
  services/    import (doublons, cohérence), titulaires (holders.rs), noms de fichiers standard (naming.rs), prêts, utilisateurs, droits et partages, contacts, catégories, prix, valorisation, taux réglementés
  app.rs       le cœur de l'application (coffre, documents, imports, relecture)
  tests/       les 298 tests de la version Python, portés un à un, plus ceux des fonctions propres à Rust
  testkit/     de quoi les écrire : générateur de PDF fictifs, relevés BoursoBank/Fortuneo/prêts, client web à cookies
  web/         serveur HTTP, routes, sessions et CSRF, Cloudflare Access, graphiques SVG, thèmes, PWA,
               et un moteur de gabarits compatible Jinja2 (les gabarits de web/templates sont ceux de la version Python)
web/           gabarits, fichiers statiques et icônes
python/        la version Python d'origine (FastAPI + Jinja), avec ses tests
tools/         comparaisons différentielles Rust ↔ Python
```

### Comparer avec la version Python

Depuis `python/` (après `pip install -e ".[dev]"`), avec le binaire Rust construit :

```bash
MYMONEY_CAPTURE=/tmp/corpus PYTHONPATH=.. python -m pytest -q -p tools.capture_docs   # garde chaque document lu par les tests
python ../tools/diff_parse.py ../target/release/mymoney $(find /tmp/corpus -type f)   # même lecture, au caractère près
cd .. && ./tools/diff_jinja.sh                                                          # mêmes gabarits que Jinja2
python3 tools/diff_http.py --docs /tmp/corpus --work /tmp/http --keep --tolerant      # même application, requête par requête
python3 tools/diff_http.py --existing /tmp/http/py/data --tolerant                     # Rust ouvre les données de la version Python
python3 tools/check_test_parity.py                                                      # chaque test Python a son équivalent Rust
```

`tools/gen_edge_cases.py` (CSV, OFX et QIF piégés) et `tools/gen_pdf_variants.py` (PDF chiffrés ou abîmés, avec pypdf)
fabriquent d'autres documents fictifs à comparer.

Depuis que la version Rust a des fonctions que la version Python n'a pas (arbre par compte, noms de fichiers
standard, titulaires en plusieurs cases, actualisation), la comparaison web est **tolérante** (`--tolerant`) : les
différences sont listées, et elle n'échoue que si Rust plante (5xx) ou refuse ce que Python accepte. La lecture des
documents (`diff_parse`) et les gabarits (`diff_jinja`) restent comparés strictement.

**Écart de lecture connu**, volontaire : un montant écrit en notation scientifique (`1e2` dans un OFX, ce qu'aucune banque
n'écrit) est gardé `1E+2` par Python et `100` par Rust. La valeur et l'affichage (« 100,00 € ») sont les mêmes.

### Écrire un adapter

1. Crée `src/adapters/<etablissement>/<type_de_document>.rs` avec un type qui implémente `Adapter` (`id`, `institution`, `label`, `formats`).
2. `detect(doc)` note de 0 à 1 à quel point le document est bien de ce type, d'après ce qui le distingue des autres documents du même établissement (titre, en-têtes de colonnes, mentions) ; il ne lit rien de personnel.
3. `parse(doc)` renvoie des `ParsedStatement` : identité du compte (titulaire, adresse, IBAN, RIB), période, soldes initial et final, et des `TransactionData` structurées (date, date de valeur, type, interlocuteur, IBAN de l'interlocuteur, référence, libellé complet dans `raw`). Les soldes servent au contrôle : opérations lues = solde initial → solde final.
4. Ajoute-le à `ADAPTERS` dans `src/adapters/registry.rs`.
5. **Teste avec un document fictif** (les tests Python dessinent de faux documents à la mise en page réelle : `python/tests/fixtures*.py`). Ne commite jamais un vrai relevé. Tant que la version Python sert de référence à la CI, un nouvel adapter s'ajoute aussi de son côté (`python/mymoney/adapters/`), sinon la comparaison différentielle signale l'écart.

Les taux réglementés se mettent à jour dans `src/services/rates.rs` (et `python/mymoney/services/rates.py` pour la référence), une ligne par changement, les 1er février et 1er août.
