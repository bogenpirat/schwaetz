// Shows 7TV, BetterTTV and FrankerFaceZ emotes inline in Twitch channels.
// @grant http

type EmoteMap = Map<string, string>; // name → image URL

const global: EmoteMap = new Map();
const channels = new Map<string, EmoteMap>(); // "#channel" → emotes
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

// Global emotes.
get("https://7tv.io/v3/emote-sets/global", (j) => add7tv(global, j));
get("https://api.betterttv.net/3/cached/emotes/global", (j) => addBttv(global, j));
get("https://api.frankerfacez.com/v1/set/global", (j) => addFfz(global, j.sets));

function loadChannel(channel: string, id: string) {
  const map: EmoteMap = new Map();
  channels.set(channel, map);
  get(`https://7tv.io/v3/users/twitch/${id}`, (j) => add7tv(map, j.emote_set));
  get(`https://api.betterttv.net/3/cached/users/twitch/${id}`, (j) => {
    addBttv(map, j.channelEmotes);
    addBttv(map, j.sharedEmotes);
  });
  get(`https://api.frankerfacez.com/v1/room/id/${id}`, (j) => addFfz(map, j.sets));
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

schwaetz.on("message", (line) => {
  if (!line.network || !line.buffer.startsWith("#")) return;
  const local = channels.get(line.buffer.toLowerCase());
  if (!local && global.size === 0) return;
  const emotes: { start: number; end: number; url: string; name: string }[] = [];
  const re = /\S+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line.plain)) !== null) {
    const word = m[0];
    const url = local?.get(word) ?? global.get(word);
    if (url) emotes.push({ start: m.index, end: m.index + word.length, url, name: word });
  }
  if (emotes.length) schwaetz.decorate(line, emotes);
});

schwaetz.command(
  "emotes",
  (ctx) => {
    const local = channels.get(ctx.buffer.toLowerCase());
    schwaetz.print(`${global.size} global and ${local?.size ?? 0} channel emotes loaded`, ctx);
  },
  "Show how many third-party emotes are loaded",
);
