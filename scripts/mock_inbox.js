// Local mock inbox for the F#12 email demo — stands in for a live
// disposable-mail provider's RSS feed + a "your link is live" page.
const http = require('http');

const RSS = `<rss version="2.0">
  <channel>
    <link>http://127.0.0.1:8180/</link>
    <title>demohermes @ demo-inbox</title>
    <description>Backlink confirmation</description>
    <item>
      <title><![CDATA[Backlink accepted &#8211; demohermes]]></title>
      <description><![CDATA[
        Thank you for your submission.
        Your backlink was accepted and is now live on our site.
        Verify it here: http://127.0.0.1:8080/
        Ref: #7
      ]]></description>
    </item>
  </channel>
</rss>`;

const PAGE = `<!doctype html><html><head><title>Backlink verified — live</title></head>
<body><h1>Backlink is LIVE</h1>
<p>Ref #7 accepted. Anchor: GSA SER demo keyword. Status: 200 OK.</p>
</body></html>`;

const srv = http.createServer((req, res) => {
  if (req.url.startsWith('/rss/')) {
    res.writeHead(200, { 'Content-Type': 'application/rss+xml' });
    res.end(RSS);
  } else if (req.url === '/healthz') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true }));
  } else {
    res.writeHead(200, { 'Content-Type': 'text/html' });
    res.end(PAGE);
  }
});
srv.listen(8180, '127.0.0.1', () => console.log('mock inbox on 127.0.0.1:8180'));
