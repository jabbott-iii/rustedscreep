/*
 * rustedscreep server feature probe, console version (P0 kickoff).
 *
 * Use: paste each of the five lines below that start with "(function" into the
 * in-game console of each target server, one at a time, and press Enter after
 * each. Each prints one JSON line tagged with its "part" (wasm1, wasm2, js, api,
 * env). Copy all five lines back into the kickoff session. The probe only reads
 * state: no intents, no Memory writes.
 *
 * Why five pastes: the open-source server backend rejects a console expression
 * whose JSON-encoded length is over 1024 characters (screeps/backend-local
 * lib/game/api/user.js, VERIFICATION_LOG V69). Each line here stays under that,
 * uses no double quotes or backslashes (so JSON encoding adds nothing), and is
 * one self-contained ES5 expression. js/probe/check.mjs enforces all of this.
 *
 * Output: wasm.* keys are 1 (accepted), 0 (rejected), null (no
 * WebAssembly.validate) or "err" (validate threw). mvp (the bare 8-byte wasm
 * header) must read 1 and badver (a version-2 header) must read 0, or the wasm
 * results are not trustworthy. js.* keys are 1 if new Function() compiled the
 * snippet; js.es5 is a control that must read 1. api.* keys are typeof results.
 * env holds Game.cpu values, Game.shard and getHeapStatistics(); non-finite
 * numbers (an unlimited private server) are reported as strings.
 *
 * The wasm test modules are wasm-feature-detect v1.10.1 (dist/esm/index.js; the
 * same bytes ship in v1.8.0 and v1.9.0), stored as hex after the common 8-byte
 * header: https://github.com/GoogleChromeLabs/wasm-feature-detect
 * As upstream does, ehExnref is tested by compiling a Module; the rest use
 * WebAssembly.validate. check.mjs decodes the hex and compares it byte for byte
 * with the reference arrays.
 *
 * wasm-feature-detect notice (the hex strings in the wasm1 and wasm2 units encode
 * its test modules; the exnref module comes from its exceptions-final detector):
 * Copyright 2019 Google Inc. All Rights Reserved.
 * Copyright 2023 Google Inc. All Rights Reserved.
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *     http://www.apache.org/licenses/LICENSE-2.0
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/* 1/5 wasm1 */
(function(){var W=typeof WebAssembly==='object'&&WebAssembly||0,M={mvp:'',badver:'',mutGlobals:'02080101610162037f010606017f0141000b07050101610301',signExt:'010401600000030201000a080106004100c01a0b',satF2I:'010401600000030201000a0c010a004300000000fc001a0b',bulkMem:'0104016000000302010005030100010a0e010c00410041004100fc0a00000b',refTypes:'010401600000030201000a07010500d0701a0b'},r={probe:'rustedscreep',part:'wasm1',v:2},k,s,b,i;for(k in M){s=(k==='badver'?'0061736d02000000':'0061736d01000000')+M[k];b=new Uint8Array(s.length/2);for(i=0;i<b.length;i++)b[i]=parseInt(s.substr(2*i,2),16);if(!W||typeof W.validate!=='function')r[k]=null;else try{r[k]=k==='ehExnref'?(new W.Module(b)?1:0):(W.validate(b)?1:0)}catch(e){r[k]=k==='ehExnref'?0:'err'}}return JSON.stringify(r);})()

/* 2/5 wasm2 */
(function(){var W=typeof WebAssembly==='object'&&WebAssembly||0,M={mvp:'',multiValue:'0106016000027f7f030201000a08010600410041000b',simd:'0105016000017b030201000a0a0108004100fd0ffd620b',eh:'010401600000030201000a080106000640190b0b',ehExnref:'010401600000030201000a10010e0002691f40010300000b000b1a0b',tailCall:'010401600000030201000a0601040012000b'},r={probe:'rustedscreep',part:'wasm2',v:2},k,s,b,i;for(k in M){s=(k==='badver'?'0061736d02000000':'0061736d01000000')+M[k];b=new Uint8Array(s.length/2);for(i=0;i<b.length;i++)b[i]=parseInt(s.substr(2*i,2),16);if(!W||typeof W.validate!=='function')r[k]=null;else try{r[k]=k==='ehExnref'?(new W.Module(b)?1:0):(W.validate(b)?1:0)}catch(e){r[k]=k==='ehExnref'?0:'err'}}return JSON.stringify(r);})()

/* 3/5 js */
(function(){var T={es5:'var a=1',arrow:'()=>1',letConst:'let a=1;const b=2',classes:'class A{}',template:'`x`',destructuring:'var{a}={a:1}',spread:'[...[1]]',generator:'function*g(){}',forOf:'for(var x of[]);',exponent:'2**3',asyncAwait:'async function f(){await 0}',objectSpread:'({...{}})',asyncIteration:'async function f(){for await(var x of[]);}',optionalCatch:'try{}catch{}',optionalChaining:'({})?.a',nullish:'null??1',bigint:'1n',logicalAssign:'var a;a??=1',numericSeparator:'1_000',classFields:'class A{x=1}',privateMethods:'class A{#m(){}}',staticBlock:'class A{static{}}'},r={probe:'rustedscreep',part:'js',v:2},k;for(k in T){try{Function(T[k]);r[k]=1;}catch(e){r[k]=0;}}return JSON.stringify(r);})()

/* 4/5 api */
(function(){var w=typeof WebAssembly==='object'&&WebAssembly||{},m=typeof RawMemory==='object'&&RawMemory||{},c=Game.cpu,r={probe:'rustedscreep',part:'api',v:2,'WebAssembly':typeof WebAssembly,'WebAssembly.validate':typeof w.validate,'WebAssembly.Module':typeof w.Module,'WebAssembly.Instance':typeof w.Instance,'WebAssembly.Memory':typeof w.Memory,'WebAssembly.Tag':typeof w.Tag,'TextDecoder':typeof TextDecoder,'TextEncoder':typeof TextEncoder,'BigInt':typeof BigInt,'Uint8Array':typeof Uint8Array,'cpu.halt':typeof c.halt,'cpu.generatePixel':typeof c.generatePixel,'cpu.getHeapStatistics':typeof c.getHeapStatistics,'cpu.setShardLimits':typeof c.setShardLimits,'cpu.unlock':typeof c.unlock,'RawMemory.segments':typeof m.segments,'RawMemory.setActiveSegments':typeof m.setActiveSegments,'InterShardMemory':typeof InterShardMemory};return JSON.stringify(r);})()

/* 5/5 env */
(function(){var c=Game.cpu,r={probe:'rustedscreep',part:'env',v:2,time:Game.time},k,h,o,t;function s(d,x){for(k in x){t=typeof x[k];if(t==='number'&&!isFinite(x[k])){d[k]=String(x[k]);}else if(t==='number'||t==='boolean'||t==='string'){d[k]=x[k];}}return d;}r.cpu=s({},{limit:c.limit,tickLimit:c.tickLimit,bucket:c.bucket,unlocked:c.unlocked,unlockedTime:c.unlockedTime});if(c.shardLimits&&typeof c.shardLimits==='object'){r.cpu.shardLimits=s({},c.shardLimits);}r.shard=Game.shard&&typeof Game.shard==='object'?s({},Game.shard):null;r.heap=null;if(typeof c.getHeapStatistics==='function'){try{h=c.getHeapStatistics();r.heap=h&&typeof h==='object'?s({},h):String(h);}catch(e){r.heap='throws';}}return JSON.stringify(r);})()
