import {createEffect,createResource,createSignal,For,Index,onCleanup,onMount,Show} from "solid-js";
import {getHermesDesk,saveHermesDesk,type ModelRoute} from "../api/hermes";
import "../styles/desk.css";
const MODE_TEXT:Record<string,string>={default:"Use the parent agent’s model for every delegated task.",model_map:"Let Lucina choose from the named model routes you configure.",profile_routes:"Delegate to existing isolated Hermes profiles, each with its own settings."};
const ACTIONS:Record<string,string>={computer_task:"Computer task",cursor_task:"Project work",graph_task:"Chart",spreadsheet_update_task:"Spreadsheet",generate_image:"Image",oracle_monitor_task:"Monitor",social_post_task:"Social task"};
export default function Desk(){
 const [data,{refetch,mutate}]=createResource(getHermesDesk);
 const [routes,setRoutes]=createSignal<ModelRoute[]>([]);
 const [defaultRoute,setDefaultRoute]=createSignal("");
 const [dirty,setDirty]=createSignal(false);
 const [busy,setBusy]=createSignal(false);
 const [notice,setNotice]=createSignal("");
 const [error,setError]=createSignal("");
 createEffect(()=>{const d=data();if(d&&!dirty()){setRoutes(d.modelRoutes.map(r=>({...r})));setDefaultRoute(d.defaultRoute||"");}});
 onMount(()=>{const timer=setInterval(()=>{if(!busy()&&!dirty()&&!document.querySelector(".desk input:focus,.desk select:focus,.desk button:focus")&&document.visibilityState==="visible")void refetch();},15000);onCleanup(()=>clearInterval(timer));});
 const count=(status:string)=>(data()?.jobs||[]).filter(j=>j.status===status).length;
 function edit(index:number,key:keyof ModelRoute,value:string){setDirty(true);setRoutes(rs=>rs.map((r,i)=>i===index?{...r,[key]:value}:r));}
 async function save(body:unknown){
  if(busy())return;
  setBusy(true);setNotice("");setError("");
  try{const updated=await saveHermesDesk(body);setDirty(false);mutate(updated);setNotice("Saved. Changes apply to new Hermes sessions.");}
  catch{setError("Couldn’t save these settings. Check the route names, model IDs and connection, then retry.");}
  finally{setBusy(false);}
 }
 return <section class="desk" aria-labelledby="desk-title"><div class="desk-inner">
  <header class="desk-head"><div><p class="desk-eyebrow">LUCINA · SECRETARY</p><h1 id="desk-title">My desk</h1><p>A clear view of my work, tools and delegation.</p></div><button class="btn" type="button" disabled={busy()||data.loading} onClick={()=>void refetch()}>Refresh</button></header>
  <Show when={data.error}><div class="desk-notice" role="alert">The desk couldn’t reach Hermes. <button type="button" class="btn" onClick={()=>void refetch()}>Try again</button></div></Show>
  <Show when={notice()}><p class="desk-notice" role="status">{notice()}</p></Show>
  <Show when={error()}><p class="desk-notice desk-notice--error" role="alert">{error()}</p></Show>
  <Show when={data()} fallback={<Show when={!data.error}><p role="status">Opening my desk…</p></Show>}>{d=><>
   <div class="desk-overview">
    <article class="desk-card desk-engine"><span class="desk-kicker">COMPUTER ENGINE</span><h2>Hermes Agent</h2><p class="desk-model">{d().model.default}</p><span class="desk-badge">{d().pluginInstalled?"Official delegation plugin enabled":"Delegation plugin unavailable"}</span></article>
    <article class="desk-card desk-stat"><span>In progress</span><strong>{count("running")+count("queued")}</strong><small>Recent Secretary tasks</small></article>
    <article class="desk-card desk-stat"><span>Finished</span><strong>{count("done")}</strong><small>Of the latest {d().jobs.length} tasks</small></article>
   </div>
   <section class="desk-card" aria-labelledby="routing-title"><div class="desk-section-head"><div><span class="desk-kicker">WORKING TOGETHER</span><h2 id="routing-title">Delegation</h2></div><span class="desk-badge">Next session</span></div>
    <div class="desk-modes" role="radiogroup" aria-label="Delegation mode"><For each={d().backends}>{mode=><button type="button" role="radio" aria-checked={d().mode===mode.name} disabled={busy()||!mode.available} classList={{"desk-mode":true,"desk-mode--active":d().mode===mode.name}} onClick={()=>void save({action:"mode",mode:mode.name})}><strong>{mode.label}</strong><span>{MODE_TEXT[mode.name]}</span><Show when={mode.reason}><small>{mode.reason}</small></Show></button>}</For></div>
    <p class="desk-hint">Running conversations keep their existing tool choices. A saved change takes effect when Lucina starts a new session.</p>
   </section>
   <section class="desk-card" aria-labelledby="models-title"><div class="desk-section-head"><div><span class="desk-kicker">CHOOSING THE RIGHT HELP</span><h2 id="models-title">Model routes</h2></div><button type="button" class="btn" disabled={busy()||routes().length>=8||!d().pluginInstalled} onClick={()=>{setDirty(true);setRoutes(r=>[...r,{name:"",description:"",model:d().model.default}]);}}>Add route</button></div>
    <Show when={routes().length} fallback={<p class="desk-hint">No named routes yet. Delegated tasks currently inherit the parent model.</p>}>
     <form onSubmit={e=>{e.preventDefault();void save({action:"models",routes:routes(),default:defaultRoute()||null});}}>
      <Index each={routes()}>{(route,index)=><fieldset class="desk-route"><legend>Route {index+1}</legend>
       <label>Name<input required pattern="[a-z][a-z0-9_-]{0,31}" maxlength="32" value={route().name} placeholder="fast" onInput={e=>edit(index,"name",e.currentTarget.value)}/></label>
       <label>Model<input required pattern="gemini-[a-zA-Z0-9.-]+" maxlength="88" value={route().model} onInput={e=>edit(index,"model",e.currentTarget.value)}/></label>
       <label class="desk-route-description">When to use it<input required maxlength="200" value={route().description} placeholder="Lookups and short summaries" onInput={e=>edit(index,"description",e.currentTarget.value)}/></label>
       <button type="button" class="btn" disabled={busy()} aria-label={"Remove route "+(route().name||index+1)} onClick={()=>{setDirty(true);setRoutes(r=>r.filter((_,i)=>i!==index));if(defaultRoute()===route().name)setDefaultRoute("");}}>Remove</button>
      </fieldset>}</Index>
      <label class="desk-default">Default for tasks with no route<select value={defaultRoute()} onChange={e=>{setDirty(true);setDefaultRoute(e.currentTarget.value);}}><option value="">Parent model</option><For each={routes().filter(r=>r.name)}>{r=><option value={r.name}>{r.name}</option>}</For></select></label>
      <p class="desk-hint">Uses the Gemini credentials already configured for Lucina. A model ID must be available to that account; saving a route does not test model access.</p>
      <button type="submit" class="btn btn--primary" disabled={busy()||!dirty()}>Save routes</button>
     </form>
    </Show>
    <Show when={dirty()&&!routes().length}><button type="button" class="btn" disabled={busy()} onClick={()=>void save({action:"models",routes:[],default:null})}>Save empty route list</button></Show>
   </section>
   <section class="desk-card"><span class="desk-kicker">SEPARATE WORKSPACES</span><h2>Profiles & spaces</h2><div class="desk-spaces"><div><strong>My desktop</strong><span>{d().sandbox}</span></div><div><strong>My Android</strong><span>{d().androidSandbox}</span></div></div>
    <Show when={d().profileRoutes.length} fallback={<p class="desk-hint">No profile routes are configured. Additional profiles must preserve Lucina’s sandbox boundaries before they can be used.</p>}><For each={d().profileRoutes}>{p=><p><strong>{p.name}</strong> · {p.description}</p>}</For></Show>
   </section>
   <section class="desk-card"><span class="desk-kicker">MY TOOLKIT</span><h2>Official Hermes plugins</h2><p class="desk-hint">Installed and enabled are separate states. Some tools need an account or another application before they can work.</p><ul class="desk-jobs"><For each={d().plugins||[]}>{plugin=><li><div><strong>{plugin.label}</strong><small>{plugin.note}</small></div><span class="desk-badge">{!plugin.installed?"Not installed":plugin.enabled?"Enabled":"Setup needed"}</span></li>}</For></ul></section>
   <section class="desk-card"><div class="desk-section-head"><div><span class="desk-kicker">RECENT WORK</span><h2>Task activity</h2></div><small>Latest 20 app tasks</small></div>
    <Show when={d().jobs.length} fallback={<p class="desk-hint">No tasks have been recorded yet.</p>}><ol class="desk-jobs"><For each={d().jobs}>{job=><li><div><strong>{ACTIONS[job.action]||"Secretary task"}</strong><small>{job.createdAt?new Date(job.createdAt).toLocaleString():"Time unavailable"}</small></div><span class="desk-badge" data-state={job.status}>{job.status}</span></li>}</For></ol></Show>
   </section>
   <p class="desk-updated">Snapshot · {new Date(d().updatedAt).toLocaleTimeString()} · refreshes every 15 seconds</p>
  </>}</Show>
 </div></section>;
}
