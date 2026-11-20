# Local add-ons of the phone page

The phone page (`companion/public/rate.html`) loads `/pwa/local.js` if it exists. The file is not versioned
(`.gitignore`), so each install can add its own features without forking the page. Without it the browser logs one
404 and nothing else changes.

`local.js` may set one function:

```js
window.TVHOME_EXT = function (place, node, item) {
  // place: 'home' (search home, once at start), 'row' (list row), 'card' (Discover deck card),
  //        'tile' (My ratings grid), 'sheet' (title details panel)
  // node:  the element the page just drew; item: the title (or its details for 'sheet')
};
```

The page calls it after drawing each element; an error in the add-on is logged and ignored. Helper files the
add-on needs go next to it in `companion/public/pwa/` (only `.js`, `.png`, `.svg` and `.webmanifest` are served),
and local tools in `tools/local/`, both ignored by git.
