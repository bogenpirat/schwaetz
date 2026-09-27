// Shows 7TV, BetterTTV and FrankerFaceZ emotes inline in Twitch channels and offers them for
// `:` emote completion.
// @grant http

type EmoteMap = Map<string, string>; // name → image URL
type Provider = "7tv" | "bttv" | "ffz";
type Sets = Record<Provider, EmoteMap>;

const newSets = (): Sets => ({ "7tv": new Map(), bttv: new Map(), ffz: new Map() });
const global: Sets = newSets();
const channels = new Map<string, Sets>(); // "#channel" → emotes by provider
const roomIds = new Map<string, string>(); // "#channel" → Twitch user id

function get(url: string, parse: (json: any) => void) {
  schwaetz.http.get(url, (res) => {
    if (res.status !== 200 || !res.body) return;
    try {
      parse(JSON.parse(res.body));
    } catch (e) {
      schwaetz.log(`7tv-emotes: bad response from ${url}: ${e}`);
    }
  });
}

function add7tv(map: EmoteMap, set: any) {
  for (const e of set?.emotes ?? []) {
    const host = e?.data?.host;
    if (host?.url) map.set(e.name, `https:${host.url}/1x.webp`);
  }
}

function addBttv(map: EmoteMap, list: any[]) {
  for (const e of list ?? []) map.set(e.code, `https://cdn.betterttv.net/emote/${e.id}/1x`);
}

function addFfz(map: EmoteMap, sets: any) {
  for (const set of Object.values<any>(sets ?? {})) {
    for (const e of set.emoticons ?? []) {
      const url = e.urls?.["1"];
      if (url) map.set(e.name, url.startsWith("//") ? `https:${url}` : url);
    }
  }
}

/** Hands one provider's emotes to the client's completion. */
function offer(provider: Provider, channel: string | null, map: EmoteMap) {
  schwaetz.setEmotes(provider, channel, [...map].map(([name, url]) => ({ name, url })));
}

function load(provider: Provider, channel: string | null, map: EmoteMap, url: string, parse: (json: any) => void) {
  get(url, (j) => {
    parse(j);
    offer(provider, channel, map);
  });
}

// Global emotes.
load("7tv", null, global["7tv"], "https://7tv.io/v3/emote-sets/global", (j) => add7tv(global["7tv"], j));
load("bttv", null, global.bttv, "https://api.betterttv.net/3/cached/emotes/global", (j) => addBttv(global.bttv, j));
load("ffz", null, global.ffz, "https://api.frankerfacez.com/v1/set/global", (j) => addFfz(global.ffz, j.sets));

function loadChannel(channel: string, id: string) {
  const sets = newSets();
  channels.set(channel, sets);
  load("7tv", channel, sets["7tv"], `https://7tv.io/v3/users/twitch/${id}`, (j) => add7tv(sets["7tv"], j.emote_set));
  load("bttv", channel, sets.bttv, `https://api.betterttv.net/3/cached/users/twitch/${id}`, (j) => {
    addBttv(sets.bttv, j.channelEmotes);
    addBttv(sets.bttv, j.sharedEmotes);
  });
  load("ffz", channel, sets.ffz, `https://api.frankerfacez.com/v1/room/id/${id}`, (j) => addFfz(sets.ffz, j.sets));
}

// ROOMSTATE carries the channel's Twitch user id.
schwaetz.on("raw", (m) => {
  if (m.command !== "ROOMSTATE") return;
  const channel = m.params[0]?.toLowerCase();
  const id = m.tags["room-id"];
  if (!channel || !id || roomIds.get(channel) === id) return;
  roomIds.set(channel, id);
  loadChannel(channel, id);
});

/** The image for a word: the channel's emotes first, then global ones. */
function lookup(channel: string, word: string): string | undefined {
  const local = channels.get(channel);
  for (const p of ["7tv", "ffz", "bttv"] as Provider[]) {
    const url = local?.[p].get(word);
    if (url) return url;
  }
  for (const p of ["7tv", "ffz", "bttv"] as Provider[]) {
    const url = global[p].get(word);
    if (url) return url;
  }
  return undefined;
}

schwaetz.on("message", (line) => {
  if (!line.network || !line.buffer.startsWith("#")) return;
  const channel = line.buffer.toLowerCase();
  const emotes: { start: number; end: number; url: string; name: string }[] = [];
  const re = /\S+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line.plain)) !== null) {
    const word = m[0];
    const url = lookup(channel, word);
    if (url) emotes.push({ start: m.index, end: m.index + word.length, url, name: word });
  }
  if (emotes.length) schwaetz.decorate(line, emotes);
});

const count = (s: Sets | undefined) => (s ? s["7tv"].size + s.bttv.size + s.ffz.size : 0);

schwaetz.command(
  "emotes",
  (ctx) => {
    const local = channels.get(ctx.buffer.toLowerCase());
    schwaetz.print(`${count(global)} global and ${count(local)} channel emotes loaded`, ctx);
  },
  "Show how many third-party emotes are loaded",
);
