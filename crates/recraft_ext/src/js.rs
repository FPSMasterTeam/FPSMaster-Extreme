//! JS extension backend (QuickJS via `rquickjs`).
//!
//! Each `.js` mod gets its own QuickJS [`Context`] (isolation + hot reload). The
//! Rust<->JS interchange is the shared all-JSON [`crate::bridge`] protocol: three
//! native globals `__rcf_cmd` / `__rcf_query` / `__rcf_hud`, and the host calls
//! global `__rcf_dispatch_*` functions to fan out to registered handlers. The
//! ergonomic `recraft.*` / `hud.*` API is built on top in the JS [`PRELUDE`].

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
/// `recraft`, `hud`, `console` API and the `__rcf_dispatch_*` fan-out on top of
/// the three native globals, wrapping each handler in try/catch.
const PRELUDE: &str = r#"
(() => {
  const RT = (globalThis.__rcf = { h: { tick:[], frame:[], load:[], key:[], packet:{},
    blockChange:[], chunkLoad:[], chunkUnload:[], chat:[], entitySpawn:[], entityRemove:[],
    playerHealth:[], hud:[] } });
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
  const recraft = globalThis.recraft = {
    onTick:(cb)=>RT.h.tick.push(cb),
    onFrame:(cb)=>RT.h.frame.push(cb),
    onLoad:(cb)=>RT.h.load.push(cb),
    onKey:(cb)=>RT.h.key.push(cb),
    onChat:(cb)=>RT.h.chat.push(cb),
    onBlockChange:(cb)=>RT.h.blockChange.push(cb),
    onChunkLoad:(cb)=>RT.h.chunkLoad.push(cb),
    onChunkUnload:(cb)=>RT.h.chunkUnload.push(cb),
    onEntitySpawn:(cb)=>RT.h.entitySpawn.push(cb),
    onEntityRemove:(cb)=>RT.h.entityRemove.push(cb),
    onPlayerHealth:(cb)=>RT.h.playerHealth.push(cb),
    onPacket:(type,cb)=>{ const k=String(type); (RT.h.packet[k]||(RT.h.packet[k]=[])).push(cb); },
    drawHud:(cb)=>RT.h.hud.push(cb),
    sendChat:(s)=>cmd({t:'chat',s:String(s)}),
    sendPacket:(p)=>cmd({t:'packet',p:p}),
    log:(...a)=>cmd({t:'log',l:2,m:a.map(String).join(' ')}),
    warn:(...a)=>cmd({t:'log',l:1,m:a.map(String).join(' ')}),
    error:(...a)=>cmd({t:'log',l:0,m:a.map(String).join(' ')}),
    spawnParticle:(kind,x,y,z,o)=>{o=o||{};cmd({t:'particle',kind:kind|0,x:+x,y:+y,z:+z,
      ox:+(o.ox||0),oy:+(o.oy||0),oz:+(o.oz||0),speed:+(o.speed||0),count:(o.count|0)||1});},
    playSound:(event,x,y,z,o)=>{o=o||{};cmd({t:'sound',event:String(event),x:+x,y:+y,z:+z,
      volume:+(o.volume==null?1:o.volume),pitch:+(o.pitch==null?1:o.pitch)});},
    player:()=>JSON.parse(__rcf_query('{"k":"player"}')),
    blockAt:(x,y,z)=>JSON.parse(__rcf_query(JSON.stringify({k:'block',x:x|0,y:y|0,z:z|0}))),
    entities:()=>JSON.parse(__rcf_query('{"k":"entities"}')),
    worldTime:()=>+__rcf_query('{"k":"time"}'),
    dimension:()=>+__rcf_query('{"k":"dim"}'),
    setBlockTint:(id,c,meta)=>cmd({t:'render',r:'blockTint',id:id|0,
      meta:(meta==null?-1:meta|0),color:color(c)}),
    fullbright:(on)=>cmd({t:'render',r:'fullbright',on:!!on}),
    blockOutline:(on)=>cmd({t:'render',r:'blockOutline',on:!!on}),
    chunkBorders:(on)=>cmd({t:'render',r:'chunkBorders',on:!!on}),
    entityBox:(filter,c,on)=>cmd({t:'render',r:'entityBox',filter:String(filter||''),
      color:color(c),on:on!==false}),
    nametagScale:(s)=>cmd({t:'render',r:'nametagScale',v:+s}),
    particleDensity:(s)=>cmd({t:'render',r:'particleDensity',v:+s}),
  };
  globalThis.console = { log:recraft.log, info:recraft.log, warn:recraft.warn, error:recraft.error };
  globalThis.hud = {
    rect:(x,y,w,h,c)=>__rcf_hud(JSON.stringify({o:'rect',x:x|0,y:y|0,w:w|0,h:h|0,c:color(c)})),
    text:(x,y,text,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'text',x:x|0,y:y|0,
      s:(o.scale|0)||1,c:color(o.color),text:String(text),sh:o.shadow===false?0:1}));},
    itemIcon:(x,y,id,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'item',x:x|0,y:y|0,
      sz:(o.size|0)||16,id:id|0}));},
    blockItem:(x,y,id,meta,o)=>{o=o||{};__rcf_hud(JSON.stringify({o:'block',x:x|0,y:y|0,
      sz:(o.size|0)||16,id:id|0,meta:meta|0}));},
  };
  const safe=(cb,arg,who)=>{ try{ return cb(arg); }catch(e){ recraft.error('['+who+'] '+((e&&e.stack)||e)); } };
  globalThis.__rcf_dispatch_tick=()=>{ for(const cb of RT.h.tick) safe(cb,undefined,'onTick'); };
  globalThis.__rcf_dispatch_frame=()=>{ for(const cb of RT.h.frame) safe(cb,undefined,'onFrame'); };
  globalThis.__rcf_dispatch_load=()=>{ for(const cb of RT.h.load) safe(cb,undefined,'onLoad'); };
  globalThis.__rcf_dispatch_key=(json)=>{ const e=JSON.parse(json); let consumed=false;
    for(const cb of RT.h.key){ if(safe(cb,e,'onKey')===true) consumed=true; } return consumed; };
  globalThis.__rcf_dispatch_packet=(json)=>{ const e=JSON.parse(json); let drop=false;
    const list=RT.h.packet[e.type]; if(list) for(const cb of list){ if(safe(cb,e,'onPacket')===false) drop=true; }
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

    /// Build a [`JsPlugin`] from a mod id and its source. Registers the native
    /// globals, evals the prelude, then evals the mod source (a syntax/runtime
    /// error at load is reported and the mod rejected).
    pub fn load(&self, id: &str, source: &str) -> Result<JsPlugin, JsError> {
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
