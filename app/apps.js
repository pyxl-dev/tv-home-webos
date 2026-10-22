// Apps shown on first launch, before the Mac has a saved list
window.DEFAULT_APPS = ["netflix", "youtube.leanback.v4", "amazon", "com.disney.disneyplus-prod", "spotify-beehive"];

// How each app looks. `match` is tested against the webOS app id, so apps
// learned from the TV (opened once from the LG home) get the right style.
// Logos are sized by height (logoScale) or, for wide wordmarks, by width (logoWidth, % of the tile).
window.APP_STYLES = [
  { match: /^netflix$/, name: "Netflix", logo: "logos/netflix.svg", tile: "linear-gradient(180deg,#1f1f1f,#0c0c0c)" },
  { match: /^youtube\.leanback/, name: "YouTube", logo: "logos/youtube.svg", tile: "#fff" },
  { match: /^amazon$|primevideo/, name: "Prime Video", logo: "logos/primevideo.svg", logoScale: 1.6, tile: "linear-gradient(160deg,#1a98ff,#0f79d0)" },
  { match: /disneyplus/, name: "Disney+", word: "Disney+", tile: "radial-gradient(120% 120% at 50% 0%,#1c5fae,#0b1a46 70%)" },
  { match: /spotify/, name: "Spotify", logo: "logos/spotify.svg", tile: "linear-gradient(180deg,#232323,#121212)" },
  { match: /twitch/, name: "Twitch", logo: "logos/twitch.svg", tile: "#9146ff" },
  { match: /crunchyroll/, name: "Crunchyroll", logo: "logos/crunchyroll.svg", tile: "#f47521" },
  { match: /molotov/, name: "Molotov", word: "molotov", tile: "#ffd400", fg: "#1d1d1d" },
  { match: /france[.\-]?tv|pluzz/, name: "france.tv", logo: "logos/francetv.svg", logoWidth: 78, tile: "#101010" },
  { match: /tf1/, name: "TF1+", logo: "logos/tf1plus.svg", logoWidth: 66, tile: "radial-gradient(120% 140% at 10% 100%,#050131,#0a1290)" },
  { match: /m6|6play/, name: "M6+", logo: "logos/m6plus.svg", logoWidth: 60, tile: "#ffffff" },
  { match: /canal/, name: "Canal+", word: "CANAL+", tile: "#000" },
  { match: /arte/, name: "Arte", word: "arte", tile: "#fa481c" },
  { match: /deezer/, name: "Deezer", word: "deezer", tile: "#a238ff" },
  { match: /plex/, name: "Plex", word: "plex", tile: "#1f2326", fg: "#ebaf00" }
];

// System apps that never belong in the "Ajouter" list
window.HIDDEN_APPS = /^com\.(webos|palm|lge|lg)\.|^com\.yoan\.home$|lgchannels|^undefined$/;

window.SOURCES = [
  { id: "com.webos.app.livetv", name: "TV", glyph: "tv" },
  { id: "com.webos.app.hdmi1", name: "HDMI 1", glyph: "hdmi" },
  { id: "com.webos.app.hdmi2", name: "HDMI 2", glyph: "hdmi" },
  { id: "com.webos.app.hdmi3", name: "HDMI 3", glyph: "hdmi" },
  { id: "com.webos.app.browser", name: "Web", glyph: "web" },
  { id: "com.palm.app.settings", name: "Réglages", glyph: "gear" }
];
