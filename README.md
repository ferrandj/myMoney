# myMoney

Gestionnaire de patrimoine personnel **auto-hébergé** (NAS, Docker) : comptes bancaires, livrets, assurance-vie, bourse, crypto, immobilier. Tout est **chiffré au repos** et **aucune banque n'est connectée** : tu déposes tes relevés (PDF, CSV, OFX…), des **adapters** les lisent, et l'application en tire les comptes, leurs titulaires, leurs RIB et leurs opérations.

## Fonctionnalités

- **Dépose un fichier, le bon adapter est choisi tout seul.** Chaque adapter lit un type de document d'un établissement ; un établissement peut en avoir plusieurs (relevé mensuel PDF, export CSV, relevé d'épargne…). En cas de doute (deux lectures plausibles, établissement reconnu mais document inconnu), l'application te demande laquelle utiliser.
- **Copier ses coordonnées en un clic** : sur la fiche d'un compte, un bouton « Copier » à côté du titulaire, de l'adresse, de l'IBAN (copié sans espaces), du BIC et du RIB, et « Tout copier » pour envoyer titulaire + IBAN + BIC + RIB d'un coup. Même chose pour l'IBAN des contacts. (Sans HTTPS, le navigateur interdit l'accès direct au presse-papiers : l'application passe alors par la méthode de secours, qui marche aussi.)
- **Masquer les données sensibles** : le bouton « Masquer » de la barre du haut floute les montants, les IBAN et RIB, les noms, adresses, libellés d'opérations, noms de fichiers et les graphiques (infobulles comprises), et empêche de les sélectionner. Le mode reste actif sur l'appareil jusqu'à « Afficher ».
- **20 styles** (bouton « Style ») : 10 clairs (Clair, Neige, Sable, Menthe, Lavande, Ciel, Rose poudré, Papier, Solarized clair, Agrume) et 10 sombres (Sombre, Minuit, Ardoise, Charbon, Forêt, Aubergine, Nord, Dracula, Solarized sombre, Noir OLED), plus « Automatique » qui suit l'appareil. Le contraste de chaque style est vérifié par les tests (texte, liens, montants, graphiques). Le choix est gardé sur l'appareil. Les couleurs sont définies dans `mymoney/web/themes.py`.
- **Documents classés comme un dossier** : établissement → type de document (relevé de compte, récapitulatif des frais, offre de prêt…) → année → fichier. On clique pour déplier, on coche les fichiers voulus (ou tout un groupe) et « Télécharger la sélection » donne le fichier, ou un zip s'il y en a plusieurs.
- **Où part l'argent** (Analyses → Dépenses) : chaque opération reçoit une catégorie (Courses, Restaurants, Transports, Énergie et eau, Abonnements, Santé, Impôts, Crédit, Frais bancaires…) deviné d'après le libellé et le type d'opération, sans rien envoyer à personne. On voit le total par catégorie (anneau et barres), les dépenses mois par mois, puis, en cliquant une catégorie, **chez qui** (par bénéficiaire) et **quand** (chaque opération). Si une catégorie est fausse, tu la changes pour un bénéficiaire : c'est retenu pour toutes ses opérations, y compris les prochaines (réglage personnel de chaque utilisateur ; « Automatique » le retire). « Autres dépenses » regroupe ce qui n'a pas été reconnu. Même vue pour les revenus, et un clic sur un mois de « Entrées / sorties » ouvre le détail de ce mois.
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
- **Comptes joints** : la banque coupe le nom du titulaire (« M DUPONT OU MME LEROY CLAIR… ») ; l'application te demande les noms complets une seule fois, les garde, et reconnaît quand même le nom coupé dans les relevés de tes autres comptes.
- **Documents originaux conservés chiffrés** (illisibles sur le disque) et **téléchargeables** depuis l'application.
- **Analyses** (onglet dédié, graphiques SVG générés sur le NAS, sans bibliothèque externe) :
  - *Évolution* : courbe du patrimoine, au total, par compte ou par catégorie, sur 1, 3, 6 mois, 1 an ou tout l'historique. L'historique de chaque compte est reconstitué à partir du solde des relevés et des opérations importées ;
  - *Répartition* : anneau par catégorie ou par compte ;
  - *Entrées / sorties* : barres mensuelles, avec ou sans les virements entre tes propres comptes ;
  - *Dépenses* : par type d'opération, où part l'argent et d'où il vient (regroupé par contact) ;
  - choix des comptes pris en compte (la somme totale, les actifs, les dettes et la variation suivent la sélection) et tableau des comptes triable par colonne, avec total. La variation ne compte que les comptes déjà suivis au début de la période : ajouter un compte n'est pas de la croissance.
- **Patrimoine net** avec répartition (liquidités, épargne, investissements, crypto, immobilier, dettes) et historique.
- **Taux réglementés automatiques** : Livret A, LDDS, LEP, CEL, Livret Jeune. Un taux saisi sur le compte reste prioritaire.
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

```bash
python -m venv .venv && . .venv/bin/activate
pip install -e ".[dev]"
pytest
MYMONEY_DATA_DIR=./data mymoney serve   # http://127.0.0.1:8000
```

Structure :

```
mymoney/
  security/    crypto (AES-GCM, Argon2id, HKDF), trousseau de clés, coffre en mémoire
  db.py        base SQLCipher ouverte uniquement quand le coffre est déverrouillé
  adapters/    un adapter par type de document et par établissement
    base.py        contrat (Adapter, Detection, SourceDocument, ParsedStatement)
    registry.py    choix automatique de l'adapter (score de chaque adapter, question à l'utilisateur en cas de doute)
    boursobank/    _table.py (tableau des mouvements commun), statement_pdf.py (compte courant), savings_pdf.py (PEL, épargne)
    fortuneo/      statement_pdf.py (relevé de compte ou de livret), fees_pdf.py (récapitulatif des frais), deposit_guarantee_pdf.py (lettre FGDR),
                   loan_offer_pdf.py, loan_prepayment_pdf.py, loan_schedule_pdf.py (prêt)
    revolut/       statement_csv.py : export CSV
    generic/       OFX, QIF, CSV et PDF « devinés »
  services/    import (doublons, cohérence), prêts (reconstitution et contrôles), utilisateurs, droits et partages (`access.py`, `users.py`), contacts, prix, valorisation, taux réglementés
  web/         interface (FastAPI + Jinja, sans JavaScript externe)
```

### Écrire un adapter

1. Crée `mymoney/adapters/<etablissement>/<type_de_document>.py` avec une classe qui hérite de `Adapter` (`id`, `institution`, `label`, `formats`).
2. `detect(doc)` note de 0 à 1 à quel point le document est bien de ce type, d'après ce qui le distingue des autres documents du même établissement (titre, en-têtes de colonnes, mentions) ; il ne lit rien de personnel.
3. `parse(doc)` renvoie des `ParsedStatement` : identité du compte (titulaire, adresse, IBAN, RIB), période, soldes initial et final, et des `TransactionData` structurées (date, date de valeur, type, interlocuteur, IBAN de l'interlocuteur, référence, libellé complet dans `raw`). Les soldes servent au contrôle : opérations lues = solde initial → solde final.
4. Ajoute la classe à `ADAPTERS` dans `registry.py`.
5. **Écris le test avec un document fictif** (voir `tests/fixtures.py`, `tests/fixtures_fortuneo.py` et `tests/fixtures_loan.py`, qui dessinent de faux documents à la mise en page réelle). Ne commite jamais un vrai relevé.

Les taux réglementés se mettent à jour dans `mymoney/services/rates.py` (une ligne par changement, les 1er février et 1er août).
