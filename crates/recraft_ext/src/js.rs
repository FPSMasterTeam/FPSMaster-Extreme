//! JS extension backend (QuickJS via `rquickjs`).
//!
//! Each `.js` mod gets its own QuickJS [`Context`] (isolation + hot reload). The
//! Rust<->JS interchange is the shared all-JSON [`crate::bridge`] protocol: three
//! native globals `__rcf_cmd` / `__rcf_query` / `__rcf_hud`, and the host calls
//! global `__rcf_dispatch_*` functions to fan out to registered handlers. The
//! ergonomic `mc.*` / `hud.*` API (modelled on vanilla `mc.player` / `mc.world`)
//! is built on top in the JS [`PRELUDE`].

use std::path::Path;

use rquickjs::{CatchResultExt, Context, Function, Runtime};

use crate::bridge::{self, cur};
use crate::event::Verdict;
use crate::host::{HookCtx, HostHooks};
use crate::hud::{HudCtx, HudDraw};
use crate::input::InputEvent;
use crate::packet::PacketView;
use crate::view::ReadViews;

/// JS bootstrap eval'd into every mod context before the mod source. Builds the
/// ergonomic `mc.*` / `hud.*` / `console` API (modelled on vanilla `mc.player`,
/// `mc.world`, `mc.connection`) and the `__rcf_dispatch_*` fan-out on top of the
/// three native globals, wrapping each handler in try/catch.
const PRELUDE: &str = r#"
(() => {
  const RT = (globalThis.__rcf = { h: { tick:[], frame:[], load:[], key:[],
    sb:{}, packet:{}, blockChange:[], chunkLoad:[], chunkUnload:[], chat:[],
    entitySpawn:[], entityRemove:[], playerHealth:[], hud:[] },
    keys:[], sched:[], schedId:0 });
  const color = (c) => {
    if (c == null) return 0xffffffff;
    if (typeof c === 'number') return c >>> 0;
    if (Array.isArray(c)) { const r=c[0]|0,g=c[1]|0,b=c[2]|0,a=(c[3]==null?255:c[3]|0);
      return (((r&255)*16777216)+((g&255)<<16)+((b&255)<<8)+(a&255))>>>0; }
    if (typeof c === 'string' && c[0]==='#') { let h=c.slice(1); if(h.length===6)h+='ff';
      return parseInt(h,16)>>>0; }
    return 0xffffffff;
  };
  const cmd = (o) => __rcf_cmd(JSON.stringify(o));
  const q = (k,o) => JSON.parse(__rcf_query(JSON.stringify(Object.assign({k:k}, o||{}))));
  const q1 = (k) => __rcf_query('{"k":"'+k+'"}');
  const eid = (e) => (e && e.id != null ? e.id : e) | 0;

  // ---- mc.player: a fresh snapshot each access, with action methods ----
  const buildPlayer = () => {
    const p = q('player');
    p.heldItem    = () => q('held');
    p.inventory   = () => q('inventory');
    p.selectedSlot= () => +q1('selectedSlot');
    p.capabilities= () => q('capabilities');
    p.effects     = () => q('effects');
    p.xp          = () => q('xp');
    p.container   = () => q('container');
    p.setRotation = (yaw,pitch,opts)=>{opts=opts||{};
      cmd({t:'rotate',yaw:+yaw,pitch:+pitch,silent:!!opts.silent});};
    p.clearRotation = ()=>cmd({t:'clearRotate'});
    p.selectSlot  = (s)=>cmd({t:'selectSlot',slot:s|0});
    p.swing       = ()=>cmd({t:'swing'});
    p.useItem     = ()=>cmd({t:'useItem'});
    p.attack      = (e)=>cmd({t:'attack',id:eid(e)});
    p.interact    = (e,at)=>{const o={t:'interact',id:eid(e)};
      if(at){o.ax=+at[0];o.ay=+at[1];o.az=+at[2];} cmd(o);};
    p.placeBlock  = (x,y,z,face,cursor)=>{cursor=cursor||[8,8,8];
      cmd({t:'place',x:x|0,y:y|0,z:z|0,face:face|0,cx:cursor[0]|0,cy:cursor[1]|0,cz:cursor[2]|0});};
    p.dig         = (status,x,y,z,face)=>cmd({t:'dig',status:status|0,x:x|0,y:y|0,z:z|0,face:face|0});
    p.openInventory = ()=>cmd({t:'openInv'});
    p.closeContainer= ()=>cmd({t:'close'});
    p.clickSlot   = (slot,button,mode)=>cmd({t:'click',slot:slot|0,button:button|0,mode:mode|0});
    return p;
  };

  const world = {
    getBlock:(x,y,z)=>q('block',{x:x|0,y:y|0,z:z|0}),
    get time(){ return +q1('time'); },
    get dimension(){ return +q1('dim'); },
    get loadedChunks(){ return +q1('chunks'); },
    entities:()=>q('entities'),
    entity:(id)=>q('entity',{id:id|0}),
    setBlockTint:(id,c,meta)=>cmd({t:'render',r:'blockTint',id:id|0,
      meta:(meta==null?-1:meta|0),color:color(c)}),
    registerBlock:(id,o)=>{o=o||{};cmd({t:'block',id:id|0,texture:String(o.texture||'stone'),
      opaque:o.opaque!==false,alpha:+(o.alpha==null?1:o.alpha),lum:(o.luminance|0)||0,
      tint:(o.tint||null)});},
    fullbright:(on)=>cmd({t:'render',r:'fullbright',on:!!on}),
    chunkBorders:(on)=>cmd({t:'render',r:'chunkBorders',on:!!on}),
    entityBox:(filter,c,on)=>cmd({t:'render',r:'entityBox',filter:String(filter||''),
      color:color(c),on:on!==false}),
    nametagScale:(s)=>cmd({t:'render',r:'nametagScale',v:+s}),
    particleDensity:(s)=>cmd({t:'render',r:'particleDensity',v:+s}),
    spawnParticle:(kind,x,y,z,o)=>{o=o||{};cmd({t:'particle',kind:kind|0,x:+x,y:+y,z:+z,
      ox:+(o.ox||0),oy:+(o.oy||0),oz:+(o.oz||0),speed:+(o.speed||0),count:(o.count|0)||1});},
    playSound:(event,x,y,z,o)=>{o=o||{};cmd({t:'sound',event:String(event),x:+x,y:+y,z:+z,
      volume:+(o.volume==null?1:o.volume),pitch:+(o.pitch==null?1:o.pitch)});},
  };

  const connection = {
    get connected(){ return q1('connected') === 'true'; },
    sendChat:(s)=>cmd({t:'chat',s:String(s)}),
    sendPacket:(p)=>cmd({t:'packet',p:p}),
  };

  // ---- event registration (vanilla-ish mc.on) ----
  const EVENTS = { tick:'tick', frame:'frame', load:'load', key:'key', chat:'chat',
    blockChange:'blockChange', chunkLoad:'chunkLoad', chunkUnload:'chunkUnload',
    entitySpawn:'entitySpawn', entityRemove:'entityRemove', playerHealth:'playerHealth' };
  const log = (...a)=>cmd({t:'log',l:2,m:a.map(String).join(' ')});
  const warn= (...a)=>cmd({t:'log',l:1,m:a.map(String).join(' ')});
  const error=(...a)=>cmd({t:'log',l:0,m:a.map(String).join(' ')});

  const mc = globalThis.mc = {
    get player(){ return buildPlayer(); },
    world: world,
    connection: connection,
    log: log, warn: warn, error: error,
    on:(name,cb)=>{ const k=EVENTS[name];
      if(!k){ error('mc.on: unknown event "'+name+'"'); return; } RT.h[k].push(cb); },
    onPacket:(type,cb)=>{ const k=String(type); (RT.h.packet[k]||(RT.h.packet[k]=[])).push(cb); },
    onServerbound:(type,cb)=>{ const k=String(type); (RT.h.sb[k]||(RT.h.sb[k]=[])).push(cb); },
    drawHud:(cb)=>RT.h.hud.push(cb),
    // Forge-style key binding driven by the raw key stream.
    keyBinding:(name,defaultKey)=>{ const kb={ name:String(name||''), key:String(defaultKey||''),
      pressed:false, _p:[], _r:[], onPress:(cb)=>{kb._p.push(cb);return kb;},
      onRelease:(cb)=>{kb._r.push(cb);return kb;}, isPressed:()=>kb.pressed };
      RT.keys.push(kb); return kb; },
    // Tick-based scheduler (QuickJS has no timers).
    scheduler:{
      after:(ticks,cb)=>{ const t={n:ticks|0,every:false,cb:cb,id:++RT.schedId}; RT.sched.push(t); return t.id; },
      every:(ticks,cb)=>{ const p=Math.max(1,ticks|0); const t={n:p,p:p,every:true,cb:cb,id:++RT.schedId}; RT.sched.push(t); return t.id; },
      clear:(id)=>{ RT.sched=RT.sched.filter(t=>t.id!==id); },
    },
    config: (()=>{ let data={}; try{ data=JSON.parse(globalThis.__rcf_config||'{}')||{}; }catch(e){}
      const api={ get data(){return data;},
        load:(defaults)=>{ data=Object.assign({}, defaults||{}, data); return data; },
        get:(k,def)=>(k in data?data[k]:def),
        set:(k,v)=>{ data[k]=v; return api; },
        save:()=>cmd({t:'saveConfig',dir:(globalThis.__rcf_dir||'.'),json:JSON.stringify(data)}) };
      return api; })(),
  };

  globalThis.console = { log:log, info:log, warn:warn, error:error };
  globalThis.hud = {
    rect:(x,y,w,h,c)=>__rcf_hud(JSON.stringify({o:'rect',x:x|0,y:y|0,w:w|0,h:h|0,c:color(c)})),
    text:(x,y,text,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'text',x:x|0,y:y|0,
      s:(o.scale|0)||1,c:color(o.color),text:String(text),sh:o.shadow===false?0:1}));},
    itemIcon:(x,y,id,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'item',x:x|0,y:y|0,
      sz:(o.size|0)||16,id:id|0}));},
    blockItem:(x,y,id,meta,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'block',x:x|0,y:y|0,
      sz:(o.size|0)||16,id:id|0,meta:meta|0}));},
  };

  const safe=(cb,arg,who)=>{ try{ return cb(arg); }catch(e){ error('['+who+'] '+((e&&e.stack)||e)); } };
  globalThis.__rcf_dispatch_tick=()=>{
    for(const cb of RT.h.tick) safe(cb,undefined,'tick');
    if(RT.sched.length){ const due=[]; const keep=[];
      for(const t of RT.sched){ t.n--; if(t.n<=0){ due.push(t); if(t.every){ t.n=t.p; keep.push(t);} } else keep.push(t); }
      RT.sched=keep; for(const t of due) safe(t.cb,undefined,'scheduler'); }
  };
  globalThis.__rcf_dispatch_frame=()=>{ for(const cb of RT.h.frame) safe(cb,undefined,'frame'); };
  globalThis.__rcf_dispatch_load=()=>{ for(const cb of RT.h.load) safe(cb,undefined,'load'); };
  globalThis.__rcf_dispatch_key=(json)=>{ const e=JSON.parse(json); let consumed=false;
    for(const kb of RT.keys){ if(kb.key===e.key){ if(e.pressed!==kb.pressed){ kb.pressed=e.pressed;
      const list=e.pressed?kb._p:kb._r; for(const cb of list) safe(cb,kb,'keyBinding'); } } }
    for(const cb of RT.h.key){ if(safe(cb,e,'key')===true) consumed=true; } return consumed; };
  globalThis.__rcf_dispatch_packet=(json)=>{ const e=JSON.parse(json); let drop=false;
    const list=RT.h.packet[e.type]; if(list) for(const cb of list){ if(safe(cb,e,'onPacket')===false) drop=true; }
    return drop; };
  globalThis.__rcf_dispatch_serverbound=(json)=>{ const e=JSON.parse(json); let drop=false;
    const list=RT.h.sb[e.type]; if(list) for(const cb of list){ if(safe(cb,e,'onServerbound')===false) drop=true; }
    return drop; };
  globalThis.__rcf_dispatch_event=(json)=>{ const e=JSON.parse(json);
    const map={BlockChange:'blockChange',ChunkLoad:'chunkLoad',ChunkUnload:'chunkUnload',
      Chat:'chat',EntitySpawn:'entitySpawn',EntityRemove:'entityRemove',PlayerHealth:'playerHealth'};
    const key=map[e.type]; const list=key&&RT.h[key]; if(list) for(const cb of list) safe(cb,e,'on'+e.type); };
  globalThis.__rcf_dispatch_hud=(json)=>{ const ctx=JSON.parse(json);
    for(const cb of RT.h.hud) safe(cb,ctx,'drawHud'); };
})();
"#;

/// Owns the shared QuickJS runtime. Mod contexts are created from it.
pub struct JsRuntime {
    rt: Runtime,
}

impl JsRuntime {
    pub fn new() -> Result<Self, JsError> {
        let rt = Runtime::new().map_err(JsError::from_rquickjs)?;
        Ok(Self { rt })
    }

    /// Build a [`JsPlugin`] from a mod id, its source and its directory.
    /// Registers the native globals, injects the mod's config dir + persisted
    /// `config.json` (for `mc.config`), evals the prelude, then evals the mod
    /// source (a syntax/runtime error at load is reported and the mod rejected).
    pub fn load(&self, id: &str, source: &str, dir: &Path) -> Result<JsPlugin, JsError> {
        let config_json =
            std::fs::read_to_string(dir.join("config.json")).unwrap_or_else(|_| "{}".to_string());
        let dir_str = dir.to_string_lossy().to_string();
        let ctx = Context::full(&self.rt).map_err(JsError::from_rquickjs)?;
        ctx.with(|ctx| -> Result<(), JsError> {
            let g = ctx.globals();
            g.set(
                "__rcf_cmd",
                Function::new(ctx.clone(), |json: String| bridge::handle_cmd(&json))
                    .map_err(JsError::from_rquickjs)?,
            )
            .map_err(JsError::from_rquickjs)?;
            g.set(
                "__rcf_query",
                Function::new(ctx.clone(), |json: String| bridge::handle_query(&json))
                    .map_err(JsError::from_rquickjs)?,
            )
            .map_err(JsError::from_rquickjs)?;
            g.set(
                "__rcf_hud",
                Function::new(ctx.clone(), |json: String| bridge::handle_hud(&json))
                    .map_err(JsError::from_rquickjs)?,
            )
            .map_err(JsError::from_rquickjs)?;
            g.set("__rcf_dir", dir_str)
                .map_err(JsError::from_rquickjs)?;
            g.set("__rcf_config", config_json)
                .map_err(JsError::from_rquickjs)?;
            ctx.eval::<(), _>(PRELUDE)
                .catch(&ctx)
                .map_err(|e| JsError::Eval(e.to_string()))?;
            ctx.eval::<(), _>(source)
                .catch(&ctx)
                .map_err(|e| JsError::Eval(e.to_string()))?;
            Ok(())
        })?;
        Ok(JsPlugin {
            id: id.to_string(),
            ctx,
        })
    }
}

/// A single loaded JS mod (its own QuickJS context).
pub struct JsPlugin {
    id: String,
    ctx: Context,
}

impl JsPlugin {
    fn call_void(&self, name: &str, json: Option<&str>) {
        let id = &self.id;
        self.ctx.with(|ctx| {
            let Ok(f) = ctx.globals().get::<_, Function>(name) else {
                return;
            };
            let res = match json {
                Some(j) => f.call::<_, ()>((j.to_string(),)).catch(&ctx),
                None => f.call::<_, ()>(()).catch(&ctx),
            };
            if let Err(err) = res {
                log::error!("[ext:{id}] dispatcher {name}: {err}");
            }
        });
    }

    fn call_bool(&self, name: &str, json: &str) -> bool {
        let id = &self.id;
        self.ctx.with(|ctx| {
            let Ok(f) = ctx.globals().get::<_, Function>(name) else {
                return false;
            };
            match f.call::<_, bool>((json.to_string(),)).catch(&ctx) {
                Ok(b) => b,
                Err(err) => {
                    log::error!("[ext:{id}] dispatcher {name}: {err}");
                    false
                }
            }
        })
    }
}

impl HostHooks for JsPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn on_load(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.call_void("__rcf_dispatch_load", None);
    }

    fn on_clientbound_packet(&mut self, packet: &PacketView, ctx: &mut HookCtx) -> Verdict {
        let json = bridge::packet_view_json(packet);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        if self.call_bool("__rcf_dispatch_packet", &json) {
            Verdict::Drop
        } else {
            Verdict::Pass
        }
    }

    fn on_serverbound_packet(&mut self, packet: &PacketView, ctx: &mut HookCtx) -> Verdict {
        let json = bridge::packet_view_json(packet);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        if self.call_bool("__rcf_dispatch_serverbound", &json) {
            Verdict::Drop
        } else {
            Verdict::Pass
        }
    }

    fn on_event(&mut self, event: &crate::event::ExtEvent, ctx: &mut HookCtx) {
        let json = bridge::event_json(event);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.call_void("__rcf_dispatch_event", Some(&json));
    }

    fn on_tick(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.call_void("__rcf_dispatch_tick", None);
    }

    fn on_frame(&mut self, ctx: &mut HookCtx) {
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.call_void("__rcf_dispatch_frame", None);
    }

    fn on_input(&mut self, input: &InputEvent, ctx: &mut HookCtx) -> bool {
        let json = bridge::input_json(input);
        let (views, commands) = ctx.raw_parts();
        let _v = cur::ViewsGuard::enter(views);
        let _c = cur::CommandsGuard::enter(commands);
        self.call_bool("__rcf_dispatch_key", &json)
    }

    fn draw_hud(&mut self, hud: &mut HudDraw, ctx: &HudCtx, views: &dyn ReadViews) {
        let json = bridge::hud_ctx_json(ctx);
        let _v = cur::ViewsGuard::enter(views);
        let _h = cur::HudGuard::enter(hud);
        self.call_void("__rcf_dispatch_hud", Some(&json));
    }
}

/// Discover the mods under `dir`. Each mod is a subdirectory containing a
/// `mod.toml`; returns every parsed manifest paired with its directory (entry
/// files are resolved relative to it). Skips subdirectories without a manifest.
pub fn discover_mods(
    dir: &Path,
) -> Result<Vec<(crate::manifest::ModManifest, std::path::PathBuf)>, JsError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out), // no mods dir → no mods
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let manifest_path = path.join("mod.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let src = std::fs::read_to_string(&manifest_path)
            .map_err(|e| JsError::Io(format!("{}: {e}", manifest_path.display())))?;
        let manifest = crate::manifest::ModManifest::parse(&src)
            .map_err(|e| JsError::Manifest(format!("{}: {e}", manifest_path.display())))?;
        out.push((manifest, path));
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum JsError {
    #[error("js runtime error: {0}")]
    Runtime(String),
    #[error("js eval error: {0}")]
    Eval(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("manifest error: {0}")]
    Manifest(String),
}

impl JsError {
    fn from_rquickjs(e: rquickjs::Error) -> Self {
        JsError::Runtime(e.to_string())
    }
}
