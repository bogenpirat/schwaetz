// Classic IRC fun: /slap and a simple auto-responder for !time.

schwaetz.command("slap", (ctx) => {
  const who = ctx.argv[0];
  if (!who) return schwaetz.print("Usage: /slap <nick>", ctx);
  schwaetz.exec(`/me slaps ${who} around a bit with a large trout`, ctx);
}, "Slap someone with a trout");

schwaetz.on("message", (line) => {
  if (line.own || line.history || line.plain.trim() !== "!time") return;
  schwaetz.say(line.network, line.buffer, `It is ${new Date().toUTCString()}`);
});
