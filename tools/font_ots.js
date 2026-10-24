const { chromium } = require('playwright-core');
const fs = require('fs');
(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome', args: ['--no-sandbox'] });
  const p = await b.newPage();
  for (const f of process.argv.slice(2)) {
    const data = fs.readFileSync(f).toString('base64');
    const r = await p.evaluate(async ([d, f]) => {
      try {
        const bin = Uint8Array.from(atob(d), c => c.charCodeAt(0));
        const ff = new FontFace('t', bin.buffer);
        await ff.load();
        return 'ok ' + ff.status;
      } catch (e) { return 'FAIL ' + e; }
    }, [data, f]);
    console.log(f, r);
  }
  await b.close();
})();
