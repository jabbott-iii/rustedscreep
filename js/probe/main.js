/*
 * rustedscreep server feature probe, module version (P0 kickoff).
 *
 * Two ways to use it:
 * - Without replacing your code: add this file as an extra module (for example
 *   named rsprobe) next to main, then type require('rsprobe').run() in the
 *   console. It returns the same five JSON lines as console-snippet.js.
 * - On a private server: deploy it as main. loop() logs each line as
 *   "rustedscreep-probe <json>" once per global (the first tick after every
 *   code load or global reset).
 * WARNING: deploying it as main REPLACES the code running on that branch. Never
 * do that on an official server's active branch.
 *
 * The five probe bodies below must stay identical, in order, to the five lines
 * in console-snippet.js. js/probe/check.mjs compares them on every run. See
 * console-snippet.js for the output format.
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
var PARTS = [];
PARTS.push(function(){var W=typeof WebAssembly==='object'&&WebAssembly||0,M={mvp:'',badver:'',mutGlobals:'02080101610162037f010606017f0141000b07050101610301',signExt:'010401600000030201000a080106004100c01a0b',satF2I:'010401600000030201000a0c010a004300000000fc001a0b',bulkMem:'0104016000000302010005030100010a0e010c00410041004100fc0a00000b',refTypes:'010401600000030201000a07010500d0701a0b'},r={probe:'rustedscreep',part:'wasm1',v:2},k,s,b,i;for(k in M){s=(k==='badver'?'0061736d02000000':'0061736d01000000')+M[k];b=new Uint8Array(s.length/2);for(i=0;i<b.length;i++)b[i]=parseInt(s.substr(2*i,2),16);if(!W||typeof W.validate!=='function')r[k]=null;else try{r[k]=k==='ehExnref'?(new W.Module(b)?1:0):(W.validate(b)?1:0)}catch(e){r[k]=k==='ehExnref'?0:'err'}}return JSON.stringify(r);});
PARTS.push(function(){var W=typeof WebAssembly==='object'&&WebAssembly||0,M={mvp:'',multiValue:'0106016000027f7f030201000a08010600410041000b',simd:'0105016000017b030201000a0a0108004100fd0ffd620b',eh:'010401600000030201000a080106000640190b0b',ehExnref:'010401600000030201000a10010e0002691f40010300000b000b1a0b',tailCall:'010401600000030201000a0601040012000b'},r={probe:'rustedscreep',part:'wasm2',v:2},k,s,b,i;for(k in M){s=(k==='badver'?'0061736d02000000':'0061736d01000000')+M[k];b=new Uint8Array(s.length/2);for(i=0;i<b.length;i++)b[i]=parseInt(s.substr(2*i,2),16);if(!W||typeof W.validate!=='function')r[k]=null;else try{r[k]=k==='ehExnref'?(new W.Module(b)?1:0):(W.validate(b)?1:0)}catch(e){r[k]=k==='ehExnref'?0:'err'}}return JSON.stringify(r);});
PARTS.push(function(){var T={es5:'var a=1',arrow:'()=>1',letConst:'let a=1;const b=2',classes:'class A{}',template:'`x`',destructuring:'var{a}={a:1}',spread:'[...[1]]',generator:'function*g(){}',forOf:'for(var x of[]);',exponent:'2**3',asyncAwait:'async function f(){await 0}',objectSpread:'({...{}})',asyncIteration:'async function f(){for await(var x of[]);}',optionalCatch:'try{}catch{}',optionalChaining:'({})?.a',nullish:'null??1',bigint:'1n',logicalAssign:'var a;a??=1',numericSeparator:'1_000',classFields:'class A{x=1}',privateMethods:'class A{#m(){}}',staticBlock:'class A{static{}}'},r={probe:'rustedscreep',part:'js',v:2},k;for(k in T){try{Function(T[k]);r[k]=1;}catch(e){r[k]=0;}}return JSON.stringify(r);});
PARTS.push(function(){var w=typeof WebAssembly==='object'&&WebAssembly||{},m=typeof RawMemory==='object'&&RawMemory||{},c=Game.cpu,r={probe:'rustedscreep',part:'api',v:2,'WebAssembly':typeof WebAssembly,'WebAssembly.validate':typeof w.validate,'WebAssembly.Module':typeof w.Module,'WebAssembly.Instance':typeof w.Instance,'WebAssembly.Memory':typeof w.Memory,'WebAssembly.Tag':typeof w.Tag,'TextDecoder':typeof TextDecoder,'TextEncoder':typeof TextEncoder,'BigInt':typeof BigInt,'Uint8Array':typeof Uint8Array,'cpu.halt':typeof c.halt,'cpu.generatePixel':typeof c.generatePixel,'cpu.getHeapStatistics':typeof c.getHeapStatistics,'cpu.setShardLimits':typeof c.setShardLimits,'cpu.unlock':typeof c.unlock,'RawMemory.segments':typeof m.segments,'RawMemory.setActiveSegments':typeof m.setActiveSegments,'InterShardMemory':typeof InterShardMemory};return JSON.stringify(r);});
PARTS.push(function(){var c=Game.cpu,r={probe:'rustedscreep',part:'env',v:2,time:Game.time},k,h,o,t;function s(d,x){for(k in x){t=typeof x[k];if(t==='number'&&!isFinite(x[k])){d[k]=String(x[k]);}else if(t==='number'||t==='boolean'||t==='string'){d[k]=x[k];}}return d;}r.cpu=s({},{limit:c.limit,tickLimit:c.tickLimit,bucket:c.bucket,unlocked:c.unlocked,unlockedTime:c.unlockedTime});if(c.shardLimits&&typeof c.shardLimits==='object'){r.cpu.shardLimits=s({},c.shardLimits);}r.shard=Game.shard&&typeof Game.shard==='object'?s({},Game.shard):null;r.heap=null;if(typeof c.getHeapStatistics==='function'){try{h=c.getHeapStatistics();r.heap=h&&typeof h==='object'?s({},h):String(h);}catch(e){r.heap='throws';}}return JSON.stringify(r);});
module.exports.run = function () {
  var out = [];
  var i;
  for (i = 0; i < PARTS.length; i++) {
    out.push(PARTS[i]());
  }
  return out.join('\n');
};
var reported = false;
module.exports.loop = function () {
  var i;
  if (!reported) {
    reported = true;
    for (i = 0; i < PARTS.length; i++) {
      console.log('rustedscreep-probe ' + PARTS[i]());
    }
  }
};
