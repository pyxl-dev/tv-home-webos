# Agent sur le Mac qui ne joint pas la télé (Réseau local macOS)

Depuis macOS 15, une app doit être autorisée dans Réglages Système → Confidentialité et sécurité → Réseau local
pour parler aux appareils du LAN. macOS reconnaît l'app par sa signature de code : une app mal signée peut être
« autorisée » dans les réglages et quand même bloquée par le noyau.

## Symptôme

Depuis un agent lancé par une app de bureau (ex. Hermes Desktop), tout échoue vers la télé :

```
ping: sendto: No route to host
nc: connectx to 192.168.1.50 port 3001 (tcp) failed: No route to host
node tools/tv.mjs shot  →  TV unreachable (off?)
```

alors que les mêmes commandes passent depuis un autre terminal (Claude, Ghostty) ou via le gateway Telegram de Hermes.
Ce n'est ni l'IP, ni la télé, ni la config de l'agent.

## Diagnostic

Relancer la commande depuis l'agent, puis lire les décisions Réseau local des dernières minutes :

```sh
/usr/bin/log show --last 10m --style compact --info \
  --predicate 'subsystem == "com.apple.networkextension" AND eventMessage CONTAINS[c] "local network"'
```

Signature du problème : un `Got local network blocked notification: pid: <pid de l'app>` suivi de
`Local network allowed by preference for <App>`. L'autorisation existe mais ne s'applique pas au processus.

Vérifier ensuite la signature de l'app :

```sh
codesign -dv <chemin>/Hermes.app 2>&1 | grep -E 'Identifier|flags|TeamIdentifier'
```

`Identifier=Electron` et `flags=…(adhoc,linker-signed)` : l'app n'a jamais été signée, seul le binaire Electron
porte la signature minimale du linker. macOS voit `Electron` au lancement et `Hermes` dans les réglages.

## Correctif (signature ad hoc)

1. Quitter complètement l'app (Cmd+Q).
2. La signer ad hoc :

   ```sh
   codesign --force --deep --sign - ~/.hermes/hermes-agent/apps/desktop/release/mac-arm64/Hermes.app
   ```

3. Vérifier : `codesign -dv … ` doit afficher `Identifier=com.nousresearch.hermes` et `flags=0x2(adhoc)`, et
   `codesign --verify --deep --strict …` ne rien signaler.
4. Relancer l'app, accepter la demande Réseau local si elle apparaît, refaire le `ping` depuis l'agent.

Vérifié le 2026-10-25 : avant, `No route to host` ; après, la télé répond.

**À refaire après chaque recompilation** de l'app (`hermes desktop`, `npm run pack`…) : le build local sort
non signé (aucun certificat sur le Mac) et le blocage revient.

## Option durable

Un certificat « Apple Development » gratuit (Xcode → Réglages → Comptes → Apple ID → Personal Team →
Manage Certificates → + Apple Development) donne un Team ID stable, qui devrait éviter de réautoriser l'app
à chaque build. Le build Hermes ne le détecte pas tout seul (il ne cherche que « Developer ID Application ») :
signer à la main avec `codesign --force --deep --sign "Apple Development" …/Hermes.app` après chaque build.

En attendant, passer par Telegram : le gateway Hermes (lancé via `osascript`, autorisé) n'a pas le problème.
