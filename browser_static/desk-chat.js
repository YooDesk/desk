// 自动读取聊天；面板按需打开，失败保留草稿，不自动重放发送。
window.createDeskChat = function ({request,api,state}) {
  const el=id=>document.getElementById(id);
  let generation=0,controller,timer,session,busy=false,sending=false,last=0;
  const messages=new Map();
  const valid=current=>current===generation && session===state.sessionId && !state.readonly;
  function render(items) {
    for(const message of items) messages.set(message.sequence,message);
    while(messages.size>200) messages.delete(messages.keys().next().value);
    el('chat-log').replaceChildren();
    for(const message of messages.values()) {
      const entry=document.createElement('div'); entry.className=`chat-entry${message.sender==='viewer'?' local':''}`;
      const meta=document.createElement('div'); meta.className='chat-meta'; meta.textContent=`${message.sender==='viewer'?'我':'本机'} · ${new Date(message.timestamp).toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'})}`;
      const text=document.createElement('p'); text.textContent=message.text; entry.append(meta,text); el('chat-log').append(entry);
      if(message.sequence>last && message.sender==='host' && el('chat-panel').hidden) el('chat').classList.add('unread');
      last=Math.max(last,message.sequence);
    }
    el('chat-log').scrollTop=el('chat-log').scrollHeight;
  }
  async function poll(current) {
    if(!valid(current)||busy) return; busy=true;
    try {
      const data=await request('POST',`${api}/sessions/${session}/endpoint/`,{action:'chat.read',after:last},controller.signal);
      if(!valid(current)) return;
      if(data.sequence<last) {messages.clear();last=0;render([]);}
      if(data.items.length) render(data.items);
    } catch(error) {if(valid(current)&&error.name!=='AbortError') el('chat-error').textContent=error.message;}
    finally {if(current===generation) busy=false;}
  }
  function stop() {generation+=1;clearInterval(timer);if(controller)controller.abort();controller=undefined;session=undefined;busy=false;sending=false;el('chat-send').disabled=true;}
  el('chat').addEventListener('click',()=>{el('chat-panel').hidden=!el('chat-panel').hidden;if(!el('chat-panel').hidden){el('file-panel').hidden=true;el('chat').classList.remove('unread');el('chat-input').focus();}el('chat').setAttribute('aria-pressed',String(!el('chat-panel').hidden));});
  el('chat-close').addEventListener('click',()=>{el('chat-panel').hidden=true;el('chat').setAttribute('aria-pressed','false');});
  el('chat-form').addEventListener('submit',async event=>{
    event.preventDefault();const current=generation,text=el('chat-input').value;
    if(!valid(current)||sending||!text.trim())return;
    if(new TextEncoder().encode(text).length>4096){el('chat-error').textContent='消息最多 4096 字节';return;}
    sending=true;el('chat-send').disabled=true;
    try{const data=await request('POST',`${api}/sessions/${session}/endpoint/`,{action:'chat.send',message_id:crypto.randomUUID(),text},controller.signal);if(!valid(current))return;if(el('chat-input').value===text)el('chat-input').value='';el('chat-error').textContent='';await poll(current);}
    catch(error){if(valid(current)&&error.name!=='AbortError')el('chat-error').textContent=error.message;}
    finally{if(current===generation){sending=false;el('chat-send').disabled=false;}}
  });
  el('chat-input').addEventListener('keydown',event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();el('chat-form').requestSubmit();}});
  return {start(){stop();if(state.readonly||!state.capabilities?.chat)return;session=state.sessionId;controller=new AbortController();el('chat-send').disabled=false;const current=generation;poll(current);timer=setInterval(()=>poll(current),750);},stop,clear(){stop();messages.clear();last=0;el('chat-log').replaceChildren();el('chat-input').value='';el('chat-panel').hidden=true;el('chat').classList.remove('unread');}};
};
