# myMoney

Gestionnaire de patrimoine personnel **auto-hébergé** (NAS, Docker) : comptes bancaires, livrets, assurance-vie, bourse, crypto, immobilier. Tout est **chiffré au repos** et **aucune banque n'est connectée** : tu déposes tes relevés (PDF, CSV, OFX…), des **adapters** les lisent, et l'application en tire les comptes, leurs titulaires, leurs RIB et leurs opérations.

## Fonctionnalités

- **Dépose un fichier, le bon adapter est choisi tout seul.** Chaque adapter lit un type de document d'un établissement ; un établissement peut en avoir plusieurs (relevé mensuel PDF, export CSV, relevé d'épargne…). En cas de doute (deux lectures plausibles, établissement reconnu mais document inconnu), l'application te demande laquelle utiliser.
- **Tout est rendu cohérent et contrôlé** :
  - le solde initial + les opérations doivent donner le solde final du relevé ;
  - le relevé doit prolonger le précédent (soldes qui se suivent, pas de mois manquant) ;
  - les **doublons sont signalés** : même fichier déposé deux fois, même relevé retéléchargé, relevés qui se chevauchent, opérations « probablement identiques » venues d'une autre source. Rien n'est ajouté deux fois, et tu vois ce qui a été ignoré ;
  - un import peut être **annulé** (les opérations qu'il a apportées sont retirées).
- **Identité des comptes** lue dans les documents : titulaire, adresse, IBAN, BIC, RIB (banque, guichet, compte, clé), vérifiés (clé RIB, IBAN).
- **Contacts** : à qui va l'argent, d'où il vient. Tu nommes une fois un IBAN ou un nom imprimé par la banque (avec un téléphone, des notes…) ; l'application le retient et le reconnaît dans tous les imports suivants, quelle que soit l'orthographe (« DUPONT Jean », « M Jean Dupont »). Les virements entre tes propres comptes sont repérés.
- **Documents originaux conservés chiffrés** (illisibles sur le disque) et **téléchargeables** depuis l'application.
- **Patrimoine net** avec répartition (liquidités, épargne, investissements, crypto, immobilier, dettes) et historique.
- **Taux réglementés automatiques** : Livret A, LDDS, LEP, CEL, Livret Jeune. Un taux saisi sur le compte reste prioritaire.
- **Comptes manuels** : livret, bien immobilier, crédit, positions crypto ou bourse saisies à la main, valorisées avec des prix gratuits (CoinGecko, Yahoo Finance, BCE via Frankfurter).

### Documents pris en charge

| Établissement | Adapter | Document |
|---|---|---|
| BoursoBank | `boursobank.statement_pdf` | PDF « Extrait de votre compte en EUR » (relevé mensuel) |
| Revolut | `revolut.statement_csv` | export CSV d'un compte |
| tous | `generic.ofx`, `generic.qif` | fichiers OFX / QFX et QIF |
| tous | `generic.csv`, `generic.pdf` | lecture « devinée » (colonnes date / montant / débit / crédit) : jamais importée sans que tu voies ce qui a été compris |

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
4. Ouvre `http://IP-DU-NAS:10458`, crée ton mot de passe maître et **note le code de secours**.

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

N'ouvre **pas** le port sur Internet. Utilise un VPN (Tailscale ou WireGuard, souvent disponibles sur le NAS). Pour le HTTPS en local, place l'app derrière le reverse proxy du NAS et passe `MYMONEY_COOKIE_SECURE=true`.

### Sauvegardes

Sauvegarde le dossier `data/` tel quel (Hyper Backup, rsync…). Il est chiffré : la sauvegarde l'est aussi. Garde le code de secours **ailleurs** que sur le NAS.

## Ajouter tes comptes

1. Télécharge le relevé dans l'espace client de ta banque (PDF, CSV ou OFX).
2. Va dans **Documents** et **dépose** le fichier (glisser-déposer, plusieurs fichiers possibles).
3. Le document est chiffré, reconnu, lu, puis contrôlé :
   - compte déjà connu (même IBAN / même RIB), relevé cohérent, sans doublon : **importé automatiquement** ;
   - sinon, une page de vérification montre ce qui a été compris (titulaire, RIB, soldes, opérations, doublons, anomalies) et te laisse choisir le compte, l'adapter ou ce qu'il faut importer.
4. Dans **Contacts**, nomme les bénéficiaires et émetteurs qui reviennent : ils seront reconnus automatiquement ensuite.

Un livret, un bien immobilier, un crédit ou des positions crypto sans relevé : **+ Compte manuel** sur la page Patrimoine.

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
    boursobank/    statement_pdf.py : extrait de compte mensuel
    revolut/       statement_csv.py : export CSV
    generic/       OFX, QIF, CSV et PDF « devinés »
  services/    import (doublons, cohérence), contacts, prix, valorisation, taux réglementés
  web/         interface (FastAPI + Jinja, sans JavaScript externe)
```

### Écrire un adapter

1. Crée `mymoney/adapters/<etablissement>/<type_de_document>.py` avec une classe qui hérite de `Adapter` (`id`, `institution`, `label`, `formats`).
2. `detect(doc)` note de 0 à 1 à quel point le document est bien de ce type, d'après ce qui le distingue des autres documents du même établissement (titre, en-têtes de colonnes, mentions) ; il ne lit rien de personnel.
3. `parse(doc)` renvoie des `ParsedStatement` : identité du compte (titulaire, adresse, IBAN, RIB), période, soldes initial et final, et des `TransactionData` structurées (date, date de valeur, type, interlocuteur, IBAN de l'interlocuteur, référence, libellé complet dans `raw`). Les soldes servent au contrôle : opérations lues = solde initial → solde final.
4. Ajoute la classe à `ADAPTERS` dans `registry.py`.
5. **Écris le test avec un document fictif** (voir `tests/fixtures.py`, qui dessine un faux relevé à la mise en page réelle). Ne commite jamais un vrai relevé.

Les taux réglementés se mettent à jour dans `mymoney/services/rates.py` (une ligne par changement, les 1er février et 1er août).
