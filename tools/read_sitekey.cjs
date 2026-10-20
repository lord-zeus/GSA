// Read the rendered reCAPTCHA sitekey from localendar via CDP.
const http = require('http');
function get(p){return new Promise((res,rej)=>{http.get('http://127.0.0.1:9222'+p,r=>{let d='';r.on('data',c=>d+=c);r.on('end',()=>res(d))}).on('error',rej)})}
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
  const tabs=JSON.parse(await get('/json'));
  let t=tabs.find(x=>x.type==='page');
  const ws=new WebSocket(t.webSocketDebuggerUrl);
  let id=0; const pending=new Map();
  const send=(method,params)=>new Promise(res=>{const mid=++id;pending.set(mid,res);ws.send(JSON.stringify({id:mid,method,params}));});
  ws.onmessage=(e)=>{const m=JSON.parse(e.data);if(m.id&&pending.has(m.id)){pending.get(m.id)(m.result);pending.delete(m.id);}};
  await new Promise(r=>ws.onopen=r);
  await send('Page.enable',{});
  await send('Runtime.enable',{});
  await send('Page.navigate',{url:'https://localendar.com/calendar/signup'});
  await sleep(7000);
  for(let i=0;i<10;i++){
    const r=await send('Runtime.evaluate',{expression:"location.search+' | '+ (document.querySelector('[data-sitekey]')?document.querySelector('[data-sitekey]').getAttribute('data-sitekey'):'NO_WIDGET') + ' | iframe:'+(document.querySelector('iframe[src*=recaptcha]')?document.querySelector('iframe[src*=recaptcha]').src.slice(0,80):'none')",returnByValue:true});
    const v=r&&r.result&&r.result.value;
    console.log('attempt',i,':',v);
    if(v&&v.includes('6Lc')){console.log('SITEKEY_EXTRACTED');return;}
    await sleep(1500);
  }
  ws.close();
})().catch(e=>{console.error('ERR',e.message);process.exit(1)});
