# TV Home

Accueil façon Apple TV pour une télé LG webOS (testé sur 43UP80006LR, webOS 6), sans pubs.

- `app/` — l'app webOS (`com.yoan.home`), installée en mode développeur.
- `companion/` — service Rust (tokio + axum) qui tourne en permanence sur un serveur du réseau local (ici un vieux portable sous Fedora) :
  - ouvre l'Accueil à l'allumage de la télé (et, en option, à la place de l'accueil LG) via l'API réseau officielle de LG (SSAP) ;
  - mémorise la dernière chaîne et les apps ouvertes ;
  - guide TV TNT (xmltvfr.fr) pour la carte Live TV ;
  - suggestions de films et séries (TMDB) selon tes notes (J'aime / Intéressé / Passe / Pas vu), filtrées et priorisées selon tes plateformes ;
  - redirection optionnelle du bouton maison vers l'Accueil ;
  - page web pour noter depuis le téléphone : `http://<ip-du-serveur>:8787`.

## Configurer

Deux fichiers locaux, non versionnés, à créer depuis leur modèle :

```sh
cp app/config.example.js app/config.js                    # adresse du compagnon vue par la télé
cp companion/tv-home.env.example companion/tv-home.env    # IP et MAC de la télé
```

Le compagnon refuse de démarrer sans `TVHOME_TV_IP`. Donne une IP fixe à la télé et au serveur dans la box.

## Installer

```sh
# App TV (device ares-cli nommé lg-tv)
ares-package app -o dist && ares-install -d lg-tv dist/*.ipk

# Compagnon (sur le serveur, service systemd utilisateur ; linger activé)
cd ~/tv-home/companion && cargo build --release
mkdir -p ~/.config/systemd/user && ln -sf ~/tv-home/companion/tv-home-companion.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now tv-home-companion
```

Le service lit `companion/tv-home.env`. Au premier lancement, accepte le jumelage sur la télé avec la vraie télécommande.

Déployer une modif depuis un autre poste (remplacer `serveur.local` par le nom du serveur) :

```sh
rsync -az --exclude target --exclude dist --exclude companion.log --exclude companion/data ./ serveur.local:~/tv-home/
ssh serveur.local 'cd ~/tv-home/companion && ~/.cargo/bin/cargo build --release && systemctl --user daemon-reload && systemctl --user restart tv-home-companion'
```

Logs : `ssh serveur.local tail -f tv-home/companion/companion.log`.

Tester sur la télé : `node tools/tv.mjs` (capture de l'écran réel, capture de l'app en 1920×1080, JS dans l'app, touches de télécommande, clic sur un bouton par son nom, extinction). Lancer sans argument pour l'aide. Il lit l'IP de la télé dans `companion/tv-home.env`.

Non versionnés : `app/config.js`, `companion/tv-home.env`, `companion/client-key.txt` (clé de jumelage télé) et `companion/data/` (clé TMDB, notes, cache).

## Licence

Code sous licence MIT (voir `LICENSE`).

- La police Inter (`app/fonts/`) est sous SIL Open Font License 1.1 (voir `app/fonts/OFL.txt`).
- Les logos de `app/logos/` (Netflix, YouTube, Spotify, TF1+, etc.) sont des marques de leurs propriétaires. Ils ne sont pas couverts par la licence MIT et servent seulement à identifier les applications sur la télé.
- Projet non officiel, sans lien avec LG Electronics.
