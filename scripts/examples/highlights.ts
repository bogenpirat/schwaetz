// Collects highlights (and private messages) into a "highlights" buffer, so nothing gets lost
// while you're away. /highlights clear empties the list.

const seen: string[] = [];

schwaetz.on("message", (line) => {
  if (line.own || line.history) return;
  const isQuery = line.network !== null && !line.buffer.startsWith("#") && !line.buffer.startsWith("&");
  if (!line.highlight && !isQuery) return;
  const when = new Date(line.time).toTimeString().slice(0, 5);
  const entry = `${when}  ${line.network ?? ""} ${line.buffer}  <${line.nick}> ${line.plain}`;
  seen.push(entry);
  schwaetz.print(entry, { buffer: "highlights" });
});

schwaetz.command(
  "highlights",
  (ctx) => {
    if (ctx.argv[0] === "clear") {
      seen.length = 0;
      schwaetz.print("Highlight list cleared.", ctx);
      return;
    }
    schwaetz.print(seen.length ? `${seen.length} highlight(s) — see the "highlights" buffer` : "No highlights yet.", ctx);
  },
  "Show or clear collected highlights",
);
