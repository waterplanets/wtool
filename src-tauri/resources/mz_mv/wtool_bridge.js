//=============================================================================
// wtool_bridge.js
//=============================================================================
/*:
 * @target MZ
 * @plugindesc 与 wtool 通信：上报当前对话原文，并按 wtool 的回写替换文本
 * @author wtool
 *
 * @help
 * 安装：
 *   1. 本文件放进 游戏目录/js/plugins/
 *      （MV 是 www/js/plugins/；本插件对 MV 同样有效）
 *   2. 打开 游戏目录/js/plugins.js，在数组里加一行：
 *        {"name":"wtool_bridge","status":true,"parameters":{}}
 *      如果之前装过 wtool_reader，把它那一行的 status 改成 false，
 *      两个一起开会对同一处挂两次钩子。
 *   3. 先启动 wtool（它在 127.0.0.1:17989 听着），再启动游戏。
 *
 * 协议（一行一条 JSON，UTF-8，\n 分隔，两边改一处必须同步另一处）：
 *   游戏 → wtool
 *     {"ev":"hello","engine":"mz"}
 *     {"ev":"text","seq":1,"lines":["…"],"text":"…"}
 *     {"ev":"closed","seq":1}
 *     {"ev":"terms","texts":["…"]}                            开机问整批菜单词条
 *     {"ev":"terms","loose":true,"label":"…","texts":["…"]}   问整批数据库文本、
 *                                                            事件文本、独特数据，退级查
 *                                                            （label 只是日志里那批的名字）
 *   wtool → 游戏
 *     {"cmd":"current"}                                       回一条当前 text
 *     {"cmd":"apply","seq":1,"from":"原文","text":"译文"}      替换该段
 *     {"cmd":"terms","map":{"原文":"译文"}}                    回翻得出的词条
 *     {"cmd":"font","font":"Microsoft YaHei"}                 换主字体（空串 = 恢复原字体）
 *
 * 取值（控制台里可直接用）：
 *   wtool.current()    当前对话框快照 { active, seq, lines, text }
 *   wtool.dictSize()   本地缓存了多少条译文
 *   wtool.clearDict()  清空本地缓存
 *   wtool.setDebug(false)  关掉控制台输出
 *   wtool.resendAll()      把四批整表重新问一遍（wtool 后开的、或者换过词典时用）
 *
 * 已知取舍：
 *   - 菜单文字（道具/技能/存档…）不走「上报 → 回写」那条道，它渲染时同步取字，
 *     等不起一趟往返。取而代之：开机把 data/System.json 里 terms 那一小张表
 *     整批发过去，拿回译文直接改写 $dataSystem.terms，之后谁读都是译文。
 *     只动了内存里的对象，磁盘上的 System.json 一个字节没改。
 *   - 数据库里的名字与说明（$dataItems / $dataSkills / $dataStates / $dataActors
 *     / $dataEnemies / $dataMapInfos / $dataSystem 的开关变量名…）走同一条道，
 *     但每条说明里混着 \n 和控制码（\C[14] 之类），词典里整段的 key 和只盖住
 *     半句的片段 key 都有，所以发过去时带 loose:true，让 wtool 那边走
 *     「整段 → 单行 → 片段」三级退级。具体覆盖到什么、刻意漏掉什么，
 *     见下面「数据库文本」那一节的注释。
 *   - 选项、说话人名、地图名、战斗事件里的对话，都不走「上报 → 回写」那条道
 *     （它们在渲染时同步取字，等不了一趟往返），所以和数据库文本一样，
 *     开机整批问回来存在本地 dict 里，各处渲染前同步取译文。
 *   - 菜单、列表这类**整串画**的窗口（含 SceneCustomMenu 那种把文字写在插件
 *     参数里、前面几条整批管道都碰不到的自定义菜单）挂在 drawText/drawTextEx
 *     上：画之前先查本地 dict，没见过的串攒一批问 wtool，回话到了让窗口重画。
 *     所以第一次翻开是日文、紧接着变中文；之后一直从 dict 同步取。
 *     刻意不挂 Bitmap.drawText —— 那是逐字画的落点，手里只有一个字符。
 *   - 某段对话「第一次」出现时，wtool 还没把译文送回来，画出来的仍是原文；
 *     等回写到达后本插件会让消息窗口重画一次，视觉上可能闪一下。
 *     之后同样的文本再出现，就会在画之前同步替换，不再闪。
 *     （整批问回来的那批文本不存在这个问题：dict 里早就有译文了。）
 *   - 上报给 wtool 的 text 永远是原文，写进游戏的才是译文，
 *     所以来回查表用的 key 不会互相污染。
 *   - 游戏自己的主字体（System.json 的 advanced.mainFontFilename）若是日文字体，
 *     cmap 里常有一批简体字是「有码位、轮廓为空」的占位。浏览器查到码位就不再
 *     往后面的字体退，画出来是一片空白，玩家看到的就是「缺字」。本插件因此在
 *     字体链最前面插一层系统中文字体（只认 CJK 表意字，见「字体」那一节）。
 *   - 事件指令里的台词可以直接写 \v[439] 这种变量引用（真台词在别处用 122 指令
 *     塞进变量），引擎是画字那一刻才展开的。本插件上报前先自己展开 \v[n]，
 *     否则上报出去的是字面量 "\v[439]"，词典里查不到，整段都翻不了。
 *     只展开变量，其余控制码（\c[2]、\i[127]）原样保留 —— 词典的 key 就是
 *     玩家真正看到的原文，里面不含控制码。
 *   - 不改 data/、不改词典文件，删掉本插件即完全恢复。
 */
(function () {
    'use strict';

    var HOST = '127.0.0.1';
    var PORT = 17989;      // 必须和 Rust 侧 engine/link.rs 的 PORT 一致
    var RETRY_MS = 3000;
    var LOG = '[wtool] ';

    // nwjs 的窗口里有完整 Node 集成，插件里能直接 require。
    var net;
    try {
        net = require('net');
    } catch (e) {
        console.warn(LOG + '拿不到 Node 的 net 模块，桥未启动');
        return;
    }

    //-------------------------------------------------------------------------
    // 状态
    //-------------------------------------------------------------------------

    // 永远是「原文」。写进 $gameMessage._texts 的译文不回灌到这里，
    // 否则下次查表和上报的 key 就变成译文了。
    //   lines / text —— 原文；
    //   shown        —— 此刻屏幕上实际画着的（dict 命中就是译文），
    //                   用来判断 wtool 回写的那句是不是已经画上去了。
    var state = { active: false, seq: 0, lines: [], text: '', shown: '' };

    // 原文 → 译文。wtool 回写过的落在这里，之后同步替换靠它。
    var dict = Object.create(null);

    // 我们自己写进游戏数据的那些「译文」。$DU 那批会被脚本读回来再 addText，
    // 于是中文会流进消息窗口、被当成原文又报一次；而中文喂给 wtool 的片段
    // 匹配是会被改写的（实测抽样 2.7%），所以这里留一份名单好认出来。
    var translated = Object.create(null);

    var debug = true;

    var socket = null;
    var rxBuf = '';

    //-------------------------------------------------------------------------
    // 同步查表
    //
    // 为什么不在这条 socket 上「问一句、等一句」：画字的是主线程（Chrome 主线程
    // ＋ Node 集成），主线程一阻塞，socket 的 data 事件就永远不会派发 —— 自己把
    // 自己锁死。所以查表那条连接必须**由另一个线程持有**，主线程只通过共享内存
    // 跟它对话。
    //
    // 实测过的两件事（探针量完就删了）：
    //   - 这条运行时里 worker_threads / SharedArrayBuffer / Atomics 都在，
    //     共享内存往返真的能跑通，稳态在微秒级；
    //   - 主线程的 Atomics.wait 被 Chrome 禁掉了（"cannot be called in this
    //     context"），所以主线程只能忙等。
    //
    // 共享内存布局：全是 JS↔JS，Rust 看不到，所以字符串按 UTF-16 码元直接搬，
    // 不做任何编码转换。
    //   ctrl[0] 请求就绪 / ctrl[1] 应答就绪 / ctrl[2] 请求长 / ctrl[3] 应答长
    //   ctrl[4] 唤醒代数 / ctrl[5] 连接活着 / ctrl[6] 命中 / ctrl[7] 请求号
    //   ctrl[8] 应答号：对不上说明是上一笔迟到的应答，丢掉重等
    //-------------------------------------------------------------------------

    var LOOKUP_PORT = 17990;    // 必须和 Rust 侧 engine/lookup.rs 的 PORT 一致
    var SYNC_TIMEOUT_MS = 50;   // 一次查表最多让游戏卡多久
    var SYNC_MAX_FAIL = 5;      // 连续这么多次超时就认怂，本轮不再试
    var SYNC_MAX_CHARS = 4096;  // 单条原文的码元上限

    var syncCtrl = null;
    var syncReq = null;
    var syncResp = null;
    var syncOn = false;
    var syncSeq = 0;
    var syncFail = 0;

    (function startSync() {
        var wt;
        try {
            wt = require('worker_threads');
        } catch (e) {
            return;  // 这版运行时没有：syncLookup 一直返回 null，退回攒批那条路
        }
        if (!wt || typeof wt.Worker !== 'function') return;
        if (typeof SharedArrayBuffer !== 'function') return;
        if (typeof Atomics !== 'object' || !Atomics) return;

        // Worker 源码用 eval 起，不往游戏目录多丢文件（装插件时少一个要维护的
        // 文件，重新安装的哈希比对也就只有一份）。
        var src = [
            'var w = require("worker_threads");',
            'var net = require("net");',
            'var d = w.workerData;',
            'var ctrl = new Int32Array(d.sab, 0, 16);',
            'var req = new Uint16Array(d.sab, 64, 4096);',
            'var resp = new Uint16Array(d.sab, 64 + 8192, 8192);',
            'var sock = null;',
            'var rx = "";',
            'var pending = 0;',
            'var gen = Atomics.load(ctrl, 4);',
            '',
            '// 回不了话也必须放行：主线程正阻塞等着，放行才能让它退回异步那条路。',
            'function release(seq) {',
            '    Atomics.store(ctrl, 3, 0);',
            '    Atomics.store(ctrl, 6, 0);',
            '    Atomics.store(ctrl, 8, seq);',
            '    Atomics.store(ctrl, 1, 1);',
            '    Atomics.notify(ctrl, 1);',
            '}',
            '',
            'function onLine(line) {',
            '    if (!pending) return;',
            '    var msg;',
            '    try { msg = JSON.parse(line); } catch (e) { return; }',
            '    if (msg.t !== "a" || msg.id !== pending) return;',
            '    var s = (msg.hit && typeof msg.text === "string") ? msg.text : "";',
            '    var n = s.length;',
            '    if (n > 8192) n = 8192;',
            '    for (var i = 0; i < n; i++) resp[i] = s.charCodeAt(i);',
            '    var seq = pending;',
            '    pending = 0;',
            '    Atomics.store(ctrl, 3, n);',
            '    Atomics.store(ctrl, 6, msg.hit ? 1 : 0);',
            '    Atomics.store(ctrl, 8, seq);',
            '    Atomics.store(ctrl, 1, 1);',
            '    Atomics.notify(ctrl, 1);',
            '}',
            '',
            'function connect() {',
            '    if (sock) return;',
            '    var s = net.createConnection({ host: "127.0.0.1", port: d.port });',
            '    sock = s;',
            '    s.setEncoding("utf8");',
            '    s.setNoDelay(true);',
            '    s.on("connect", function () { rx = ""; Atomics.store(ctrl, 5, 1); });',
            '    s.on("data", function (chunk) {',
            '        rx += chunk;',
            '        var i;',
            '        while ((i = rx.indexOf("\\n")) >= 0) {',
            '            onLine(rx.slice(0, i));',
            '            rx = rx.slice(i + 1);',
            '        }',
            '    });',
            '    s.on("error", function () {});',
            '    s.on("close", function () {',
            '        Atomics.store(ctrl, 5, 0);',
            '        if (pending) { release(pending); pending = 0; }',
            '        if (sock === s) sock = null;',
            '        setTimeout(connect, 1000);',
            '    });',
            '}',
            '',
            'function tick() {',
            '    if (Atomics.load(ctrl, 0) === 1) {',
            '        Atomics.store(ctrl, 0, 0);',
            '        var seq = Atomics.load(ctrl, 7);',
            '        var n = Atomics.load(ctrl, 2);',
            '        var s = "";',
            '        for (var i = 0; i < n; i++) s += String.fromCharCode(req[i]);',
            '        pending = seq;',
            '        if (sock && !sock.destroyed) {',
            '            try {',
            '                sock.write(JSON.stringify({ t: "q", id: seq, text: s }) + "\\n");',
            '            } catch (e) {',
            '                pending = 0;',
            '                release(seq);',
            '            }',
            '        } else {',
            '            pending = 0;',
            '            release(seq);',
            '        }',
            '    }',
            '    // 睡 <=1ms 等主线程叫醒（被 notify 就立刻醒），然后 setImmediate 交还',
            '    // 一次事件循环 —— 不交还的话这条线程一直堵着，socket 的 data 回调',
            '    // 永远跑不到。这就是本线程不能用 for(;;) 死等的原因。',
            '    gen = Atomics.load(ctrl, 4);',
            '    Atomics.wait(ctrl, 4, gen, 1);',
            '    setImmediate(tick);',
            '}',
            '',
            'connect();',
            'tick();'
        ].join('\n');

        try {
            var sab = new SharedArrayBuffer(64 + 8192 + 16384);
            syncCtrl = new Int32Array(sab, 0, 16);
            syncReq = new Uint16Array(sab, 64, 4096);
            syncResp = new Uint16Array(sab, 64 + 8192, 8192);
            // 插件一加载就起，绝不能等到第一次画字才起：那样第一笔要额外
            // 付一次 Worker 冷启动（实测 23ms），正好卡在开场。
            new wt.Worker(src, { eval: true, workerData: { sab: sab, port: LOOKUP_PORT } });
            syncOn = true;
            if (debug) console.log(LOG + '同步查表线程已起，端口 ' + LOOKUP_PORT);
        } catch (e) {
            syncOn = false;
            console.warn(LOG + '同步查表起不来，退回异步重画: ' + e.message);
        }
    })();

    function nowMs() {
        return (typeof performance === 'object' && performance.now)
            ? performance.now()
            : Date.now();
    }

    /// 画字那一刻同步问一次。问着了返回译文，否则返回 null（调用方退回攒批那条路）。
    ///
    /// 这里会**阻塞主线程**，所以三道闸都得有：登录状态检查（连接没握上手直接
    /// 返回）、超时（最多 `SYNC_TIMEOUT_MS`）、熔断（连续超时就整个关掉本轮）。
    function syncLookup(text) {
        if (!syncOn || !syncCtrl) return null;
        if (Atomics.load(syncCtrl, 5) !== 1) return null;   // 还没连上
        var n = text.length;
        if (n > SYNC_MAX_CHARS) return null;

        var i;
        for (i = 0; i < n; i++) syncReq[i] = text.charCodeAt(i);
        var seq = ++syncSeq;
        Atomics.store(syncCtrl, 2, n);
        Atomics.store(syncCtrl, 7, seq);
        Atomics.store(syncCtrl, 1, 0);      // 清掉可能残留的上一次应答
        Atomics.store(syncCtrl, 0, 1);
        // 必须改值再 notify：Atomics 的唤醒会重新核对期望值，值没变它会接着睡。
        Atomics.add(syncCtrl, 4, 1);
        Atomics.notify(syncCtrl, 4);

        var deadline = nowMs() + SYNC_TIMEOUT_MS;
        for (;;) {
            if (Atomics.load(syncCtrl, 1) === 1) {
                if (Atomics.load(syncCtrl, 8) === seq) break;
                Atomics.store(syncCtrl, 1, 0);   // 上一笔迟到的应答，丢掉重等
            }
            if (nowMs() > deadline) {
                Atomics.store(syncCtrl, 0, 0);
                if (++syncFail >= SYNC_MAX_FAIL) {
                    syncOn = false;
                    console.warn(LOG + '同步查表连续 ' + syncFail + ' 次超时，本轮退回异步重画');
                }
                return null;
            }
        }

        var len = Atomics.load(syncCtrl, 3);
        var ok = Atomics.load(syncCtrl, 6) === 1;
        Atomics.store(syncCtrl, 1, 0);
        syncFail = 0;
        if (!ok || !len) return null;
        var out = '';
        for (i = 0; i < len; i++) out += String.fromCharCode(syncResp[i]);
        return out;
    }

    //-------------------------------------------------------------------------
    // 传输
    //-------------------------------------------------------------------------

    function send(obj) {
        if (!socket || socket.destroyed) return;
        try {
            socket.write(JSON.stringify(obj) + '\n');
        } catch (e) {
            // 写失败不用管，close 事件会安排重连。
        }
    }

    function textEvent() {
        return { ev: 'text', seq: state.seq, lines: state.lines, text: state.text };
    }

    function connect() {
        if (socket) return;
        var s = net.createConnection({ host: HOST, port: PORT });
        socket = s;
        s.setEncoding('utf8');
        s.setNoDelay(true);

        s.on('connect', function () {
            rxBuf = '';
            send({ ev: 'hello', engine: 'mz' });
            // 重连时游戏可能正显示着对话框，补一条，免得错过。
            if (state.active) send(textEvent());
            // 重连时数据库早载入过了，onDatabaseLoaded 那一次已经错过，
            // 这里补问菜单词表和数据库文本（wtool 后开的、或者刚换了词典就走这条路）。
            sendTerms();
            sendDbTexts();
            sendEventTexts();
            sendDuTexts();
            if (debug) console.log(LOG + '已连上 ' + HOST + ':' + PORT);
        });

        s.on('data', function (chunk) {
            rxBuf += chunk;
            var i;
            while ((i = rxBuf.indexOf('\n')) >= 0) {
                var line = rxBuf.slice(0, i);
                rxBuf = rxBuf.slice(i + 1);
                handle(line.trim());
            }
        });

        // 不监听 error 的话，连接被拒会抛未捕获异常，游戏里会弹窗。
        s.on('error', function () {});

        s.on('close', function () {
            if (socket === s) socket = null;
            if (debug) console.log(LOG + '连接断开，' + RETRY_MS + 'ms 后重连');
            setTimeout(connect, RETRY_MS);
        });
    }

    function handle(line) {
        if (!line) return;
        var msg;
        try {
            msg = JSON.parse(line);
        } catch (e) {
            return;
        }

        if (msg.cmd === 'current') {
            if (state.active) send(textEvent());
        } else if (msg.cmd === 'apply') {
            applyTranslation(msg.seq, msg.from, msg.text);
        } else if (msg.cmd === 'terms') {
            // 四个整表都拿这张 map 试一遍：map 的键是原文，只有键和表里现有的
            // 字符串完全相同才会被换掉，所以多试一处不会误伤（菜单词条和物品名
            // 真的撞成同一个串时，两种查法给出的译文也一样）。
            var map = msg.map || {};
            mergeDict(map);
            applyTerms(map);
            applyDbTexts(map);
            applyDuTexts(map);
            // 「窗口文本」那批回话到了：已经画上去的日文不会自己变，让窗口重画。
            // 判据是 map 里有没有我们问过的那几串，免得开机那几批也白白重画一遍。
            for (var k in map) {
                if (askedDrawn[k]) {
                    refreshWindows();
                    break;
                }
            }
        } else if (msg.cmd === 'font') {
            // 目标字体：wtool 连上时下发一次，之后只在换配置时再来。
            setFont(msg.font);
        }
    }

    // 整批回的译文也进 dict：选项、说话人名、地图名这些不走消息窗口，
    // 那边等不了一趟往返，只能靠 dict 在渲染前同步取字。
    function mergeDict(map) {
        var n = 0;
        for (var k in map) {
            if (!map[k] || dict[k] === map[k]) continue;
            dict[k] = map[k];
            translated[map[k]] = true;
            n++;
        }
        return n;
    }

    // 这段是不是我们自己刚写上去的译文？逐行判：脚本拼出来的消息可能多行，
    // 每行都认得出来才算数。空行不算数（纯空文本本来就得报出去）。
    function isTranslated(text) {
        var parts = text.split('\n');
        var seen = false;
        for (var i = 0; i < parts.length; i++) {
            if (!parts[i]) continue;
            if (!translated[parts[i]]) return false;
            seen = true;
        }
        return seen;
    }

    //-------------------------------------------------------------------------
    // 写访问：wtool 回写的译文落到这里
    //-------------------------------------------------------------------------

    function applyTranslation(seq, from, text) {
        if (typeof from !== 'string' || typeof text !== 'string') return;
        if (!text) return;

        dict[from] = text;
        translated[text] = true;

        // 只在「正显示的就是这一段」时就地重画；否则等它下次出现，
        // 走 onOpen 里的同步替换，那时不会闪。
        if (!state.active || seq !== state.seq) return;

        // 屏幕上已经是这句译文了（dict 里早有、onOpen 已经换过的那种），
        // 再重画一次会把这句从第一个字重新逐字打一遍，白闪一下。
        if (state.shown === text) return;

        $gameMessage._texts = text.split('\n');
        state.shown = text;
        repaint();
    }

    // 已经画出去的字不会自己变，只能让消息窗口重来一遍。
    // 刻意调原始的 startMessage（绕过我们的钩子），否则会走回 onOpen，
    // 又上报一次、又被回写，来回打转。
    //
    // 这里千万不能拿 win.isOpen() 当条件。开窗是一帧一帧走的（每帧 +32，
    // 约 8 帧才到 255），而 updateMessage 又只在 !isOpening() 时才画字，
    // 也就是说回复到达时窗口基本都还在开 —— isOpen() 是 false，条件一加，
    // 重画永远被跳过，屏幕上就一直是原文。而 startMessage 早就把原文烤进
    // _textState 了，光改 $gameMessage._texts 没用，必须重跑一遍。
    // 能走到这里说明 state.active 且 seq 就是当前这段，重来是安全的。
    function repaint() {
        try {
            var scene = typeof SceneManager !== 'undefined' ? SceneManager._scene : null;
            var win = scene ? scene._messageWindow : null;
            if (win) _startMessage.call(win);
        } catch (e) {
            console.error(LOG + '重画失败', e);
        }
    }

    //-------------------------------------------------------------------------
    // 读访问：挂钩子
    //-------------------------------------------------------------------------

    function flatten(texts) {
        // $gameMessage._texts 是「事件指令 401 的参数」数组，
        // 每一条自身还可能含 \n，这里摊平成实际显示的行。
        // 摊平后 join('\n') 与 texts.join('\n') 恒等，词典 key 就取这个。
        var out = [];
        for (var i = 0; i < texts.length; i++) {
            var parts = String(texts[i]).split('\n');
            for (var j = 0; j < parts.length; j++) out.push(parts[j]);
        }
        return out;
    }

    // 展开 \v[n] —— 台词存在变量里的情况。
    //
    // 事件指令可以只写一句 \v[439]，真正的台词在事件开头用 122 指令塞进变量 439。
    // MZ 是**画字那一刻**才把 \v[n] 换成 $gameVariables.value(n) 的
    // （rmmz_windows.js 的 convertEscapeCharacters），而我们在 startMessage 里
    // 就上报了 —— 那会儿拿到的是字面量 "\v[439]"，拿这串查词典必然查不到，
    // 玩家看到的就是整段没翻。实测这个游戏有 50+ 张地图这么写，不是个例。
    //
    // 只展开变量这一种码：词典的 key 就是玩家真正看到的原文，里面不含任何
    // 控制码（实测词典里 \v[ / \c[ / \i[ 开头的 key 一个都没有）。别的码
    // （\c[2] 改色、\i[127] 图标）展开不了也不该展开 —— 留着原样，游戏自己会画。
    function expandVariables(text) {
        if (typeof text !== 'string' || text.indexOf('\\') < 0) return text;
        if (typeof $gameVariables === 'undefined' || !$gameVariables) return text;

        // 变量值里还可能再套一层 \v[n]，MZ 自己也是循环展开的，这里跟上。
        // 加个次数上限，万一有自引用（\v[439] 指向变量 439 自己）不会死循环。
        var guard = 0;
        while (guard++ < 10 && /\\v\[\d+\]/i.test(text)) {
            text = text.replace(/\\v\[(\d+)\]/gi, function (m, n) {
                try {
                    var v = $gameVariables.value(parseInt(n, 10));
                    return v == null ? '' : String(v);
                } catch (e) {
                    // 取不到值就原样留着：宁可这行没翻，也别把内容弄丢。
                    return m;
                }
            });
        }
        return text;
    }

    // 剥开 `"外层引号"对`，返回引号里的裸内容。尾部允许跟一串控制码
    // （`\t`、`\x[アイテム入手]` 之类），它们原样跟着 tail 返回。
    // 像「"竜のうろこ"\tx[アイテム入手]」这种物品入手横幅，裸内容就是物品名
    // `竜のうろこ`，而词典里常有这个裸 key 却没有整条带引号的 key —— 这是
    // 给「整行查不中」留的最后一退。剥不动（没有闭合引号对、或引号外还有
    // 非控制码字符）就返回 null，调用方原样保留该行，宁可漏翻也别拆错。
    //   返回 { inner: 引号里的字样（去掉了 JSON 转义）, tail: 引号之后原样 },
    //   或 null。
    function stripWrapper(s) {
        var i = s.indexOf('"');
        if (i !== 0) return null;
        // 找与开头引号配对的结尾引号：从尾往前找第一个 `"`。
        var j = s.lastIndexOf('"');
        if (j <= 0) return null;
        var inner = s.slice(i + 1, j);
        var tail = s.slice(j + 1);
        // 引号之间必须还剩字（空名没意义），且尾部只能剩控制码。
        // 控制码形如 \t、\x[アイテム入手]、\i[127]：一个 \<字母>，后面
        // 可选带一对 [...]（里边什么都能有，日文也算）。
        if (!inner) return null;
        if (!/^(\\[A-Za-z]+(?:\[[^\]]*\])?)*$/.test(tail)) return null;
        // 引号内的字可能是 JSON 转义过的（\"、\\），摊回真实可见字符再去查表。
        var decoded = inner.replace(/\\"/g, '"').replace(/\\\\/g, '\\');
        return { inner: decoded, tail: tail };
    }

    function onOpen() {
        // 先把 \v[n] 展开再摊平：展开后的才是玩家真正看到的原文，
        // 也才是词典里 key 的样子。不展开就整段查不中。
        var raw = $gameMessage._texts || [];
        var src = [];
        for (var i = 0; i < raw.length; i++) src.push(expandVariables(String(raw[i])));

        var lines = flatten(src);
        var text = lines.join('\n');

        // 有些消息不是事件指令 401 写死的，是脚本拼出来再 addText 的
        // （$DU 那批就走这条道，我们早把原文换成了译文），这里拿到的已经是中文。
        // 再往上报一次，wtool 会拿中文当原文去查词典，片段匹配能把它改出鬼来。
        // 所以认出来就原地不动 —— 屏幕上本来就是对的。
        if (isTranslated(text)) {
            if (debug) console.log(LOG + '已是译文，跳过', text);
            return;
        }

        state = { active: true, seq: state.seq + 1, lines: lines, text: text, shown: text };

        // 在 Window_Message 读 _texts 之前把内容换掉，画出来就直接是译文。
        // 整段命中最好；整段不在 dict 里（多行消息多半如此）就逐行退，
        // 能翻几行算几行 —— wtool 那条回写道上也是这么退级的，结果一致。
        var hit = dict[text];
        if (hit) {
            $gameMessage._texts = hit.split('\n');
            state.shown = hit;
        } else {
            var out = lines.slice();
            var any = false;
            for (var i = 0; i < out.length; i++) {
                var l = out[i];
                if (dict[l]) {
                    out[i] = dict[l];
                    any = true;
                    continue;
                }
                // 整行没翻中：剥掉外层引号 + 尾部控制码，按裸内容再查一次。
                // 「"竜のうろこ"\tx[アイテム入手]」这类入手横幅就是这种 ——
                // 词典里可能有裸名 key（竜のうろこ）而没有整条带码的 key，
                // 这一退才能把横幅里的名字换成中文。
                var w = stripWrapper(l);
                if (w && dict[w.inner]) {
                    out[i] = '"' + dict[w.inner] + '"' + w.tail;
                    any = true;
                }
            }
            if (any) {
                $gameMessage._texts = out;
                state.shown = out.join('\n');
            }
        }

        // 说话人名走 Window_NameBox（读 $gameMessage.speakerName()），
        // 不经过 _texts，得单独换。下一条消息 setSpeakerName 会重新赋原文，
        // 所以这次改写不会攒下去。
        var spk = $gameMessage._speakerName;
        if (spk && dict[spk]) $gameMessage._speakerName = dict[spk];

        if (debug) console.log(LOG + '#' + state.seq + (state.shown !== text ? ' 已译' : ' 原文'), text);
        send(textEvent());
    }

    function onClose() {
        if (!state.active) return;
        var seq = state.seq;
        state = { active: false, seq: seq, lines: [], text: '', shown: '' };
        send({ ev: 'closed', seq: seq });
    }

    // 对话是逐字画的，不能挂 drawText；startMessage 里 _texts 才是完整的整句。
    var _startMessage = Window_Message.prototype.startMessage;
    Window_Message.prototype.startMessage = function () {
        onOpen();
        return _startMessage.apply(this, arguments);
    };

    var _terminateMessage = Window_Message.prototype.terminateMessage;
    Window_Message.prototype.terminateMessage = function () {
        onClose();
        return _terminateMessage.apply(this, arguments);
    };

    //-------------------------------------------------------------------------
    // 字体：补上主字体画成空白的字
    //-------------------------------------------------------------------------
    //
    // 本作 System.json 的 advanced.mainFontFilename 是 Logotype.otf，一份日文字体。
    // 它 cmap 里有一批简体字的码位，但轮廓是空的 —— 实测译文用到的 3692 个不同
    // 汉字里有 359 个落在这批「空白字」上，词典里 45% 的译文至少含一个。
    // 浏览器查到「这个字体有这个码位」就不再往后面的字体退，于是画出来一片空白：
    // 「读取存档」少个「档」，「帮助」少个「帮」—— 后者的空白落在行首，看起来
    // 就像缩进错位。而「读/选/项/标/题」这类 Logotype 连码位都没有的字，反倒
    // 正常退到了系统字体，显示是好的，所以一直看不出问题出在字体上。
    //
    // 修法：自己声明一层系统中文字体，插到字体链最前面，但用 unicode-range 把它
    // 限制在 CJK 表意字上 —— 假名、标点、数字仍旧走游戏自己的字体，观感不变；
    // 表意字交给系统中文字体，空白字就有字了。实测假名（ロ/ー/ド）的墨迹像素数
    // 一模一样，确认没被换掉；而本机上没有这些字体时，整个 face 不可用，字体链
    // 自动退回原样。只写 CSS 和内存里的 fontFace 字符串，磁盘上一个字节没改。
    var CJK_LOCAL_FONTS = [
        'Microsoft YaHei', 'SimHei', 'SimSun', 'Microsoft JhengHei',
        'PingFang SC', 'Hiragino Sans GB', 'Noto Sans CJK SC',
        'Source Han Sans SC', 'WenQuanYi Micro Hei'
    ];

    try {
        var cjkSrc = CJK_LOCAL_FONTS.map(function (f) {
            return 'local("' + f + '")';
        }).join(', ');
        var cjkStyle = document.createElement('style');
        cjkStyle.textContent =
            '@font-face{font-family:wtool-cjk;src:' + cjkSrc + ';' +
            'unicode-range:U+3400-4DBF,U+4E00-9FFF,U+F900-FAFF;}';
        (document.head || document.getElementsByTagName('head')[0]).appendChild(cjkStyle);

        // numberFontFace() 就是 'rmmz-numberfont, ' + mainFontFace()，包住这一层
        // 它自动跟着变，不用再包一次。
        var _mainFontFace = Game_System.prototype.mainFontFace;
        Game_System.prototype.mainFontFace = function () {
            return 'wtool-cjk, ' + _mainFontFace.apply(this, arguments);
        };
    } catch (e) {
        // 装不上就退回原样：缺字虽然难看，但比游戏起不来强。
        console.warn(LOG + '字体补丁没装上', e);
    }

    //-------------------------------------------------------------------------
    // 目标字体：GUI 上选的，运行时由 wtool 下发
    //-------------------------------------------------------------------------
    //
    // 上面那层 CJK 补丁只补「有码位、轮廓为空」的表意字，假名和标点仍走游戏
    // 自带的日文字体。用户在 wtool 界面上可以另选一个系统中文字体，把整个主
    // 字体换掉 —— 这时假名、数字也一起换成所选字体。
    //
    // 包在 CJK 补丁外面，链子就是「用户字体, wtool-cjk, 游戏原字体」：
    // 用户字体优先（整体替换），wtool-cjk 退居兜底（补用户字体也缺的表意字），
    // 原字体垫底，前面都不可用时自动回落。钩子只装一次，之后只改 targetFont。
    var targetFont = '';

    var _mainFontFaceWithCjk = Game_System.prototype.mainFontFace;
    Game_System.prototype.mainFontFace = function () {
        var base = _mainFontFaceWithCjk.apply(this, arguments);
        // 字体名用双引号包起来，带空格的名字（Microsoft YaHei）也成立。
        return targetFont ? '"' + targetFont + '", ' + base : base;
    };

    function setFont(name) {
        var next = typeof name === 'string' ? name.trim() : '';
        if (next === targetFont) return;
        targetFont = next;
        // 已经画出去的字不会自己变，让消息窗口先跟上；其余窗口下次刷新自然生效。
        if (state.active) repaint();
        if (debug) console.log(LOG + '目标字体：' + (targetFont || '（游戏原字体）'));
    }

    //-------------------------------------------------------------------------
    // 菜单词表：data/System.json 的 terms
    //-------------------------------------------------------------------------
    //
    // 菜单文字（道具/技能/存档/选项/状态…）和对话不是一条道：它是渲染时同步从
    // $dataSystem.terms 里取的，一格都等不了 —— 没有「先画原文、回写到了再重画」
    // 的余地，菜单随时会被重画，窗口一开就定格了。
    //
    // 所以这里不挂钩子，改成「开机把整张小表问过去，拿回译文直接改写
    // $dataSystem.terms」。terms 四张子表加起来一百来条，一趟往返就完事。
    // 改写只发生在内存里的对象上，磁盘上的 System.json 一个字节没动。
    var TERM_GROUPS = ['basic', 'commands', 'params', 'messages'];

    // basic / commands / params 是数组（commands 里还有几个 null 空槽），
    // messages 是 { alwaysDash: "常時ダッシュ", … } 这样的对象，取值路径不一样。
    function termValues(group) {
        if (Array.isArray(group)) return group;
        var out = [];
        for (var k in group) out.push(group[k]);
        return out;
    }

    // 和 dbOriginals 同理：改写是原地做的（applyTerms 直接动 $dataSystem.terms），
    // 游戏不重启、wtool 重启时重连再问一遍，从表里读到的就已经是中文了 ——
    // 拿中文当 key 谁都不认，日志里「翻上」会从 81 掉到十几条，看着像坏了。
    // 第一次收齐的原文钉住，以后重连都拿它去问。
    var termOriginals = null;

    // 收齐 terms 里所有非空字符串并去重。数据库没载入时返回 null。
    function collectTerms() {
        var terms = typeof $dataSystem !== 'undefined' && $dataSystem ? $dataSystem.terms : null;
        if (!terms) return null;
        if (termOriginals) return termOriginals;

        var seen = Object.create(null);
        var out = [];
        for (var g = 0; g < TERM_GROUPS.length; g++) {
            var values = termValues(terms[TERM_GROUPS[g]]);
            for (var i = 0; i < values.length; i++) {
                var s = values[i];
                if (typeof s !== 'string' || !s || seen[s]) continue;
                seen[s] = true;
                out.push(s);
            }
        }
        if (!out.length) return out;   // 数据库还没载入，别缓存空结果
        termOriginals = out;
        return out;
    }

    function sendTerms() {
        var texts = collectTerms();
        if (!texts) return;   // 数据库还没载入，等 onDatabaseLoaded 再叫一次
        send({ ev: 'terms', label: '菜单词条', texts: texts });
        if (debug) console.log(LOG + '菜单词表 ' + texts.length + ' 条，已问 wtool');
    }

    // 把 wtool 回的「原文 → 译文」原地写进 $dataSystem.terms，返回改了几条。
    // 查不到的词条不在 map 里，原样留着（保持日文，好过留个空白）。
    function applyTerms(map) {
        var terms = typeof $dataSystem !== 'undefined' && $dataSystem ? $dataSystem.terms : null;
        if (!terms) return 0;

        var n = 0;
        for (var g = 0; g < TERM_GROUPS.length; g++) {
            var group = terms[TERM_GROUPS[g]];
            if (!group) continue;
            if (Array.isArray(group)) {
                for (var i = 0; i < group.length; i++) {
                    if (typeof group[i] === 'string' && map[group[i]]) {
                        group[i] = map[group[i]];
                        n++;
                    }
                }
            } else {
                // 键是 messageId（alwaysDash 之类），键本身不翻，只翻值。
                for (var k in group) {
                    if (typeof group[k] === 'string' && map[group[k]]) {
                        group[k] = map[group[k]];
                        n++;
                    }
                }
            }
        }
        if (debug) console.log(LOG + '菜单词条已替换 ' + n + ' 条');
        return n;
    }

    //-------------------------------------------------------------------------
    // 数据库文本：data/Items.json、States.json… 里的名字与说明
    //-------------------------------------------------------------------------
    //
    // 和菜单词条走同一条道（整批问、拿回译文原地改写），区别只在查法：
    // 说明里混着 \n 和控制码，词典里既有整段的 key、也有只盖住半句的片段 key，
    // 所以问的时候带 loose:true，wtool 那边就会走「整段 → 单行 → 片段」三级退级。
    //
    // 字段按表列，不共用一个全局字段表：description 只有物品/技能/武器/防具有，
    // message1..4 只有技能/状态有，nickname/profile 只有角色有，写在一起就会
    // 去读一堆不存在的键。
    //
    // 两处刻意不收：
    //   - note：那是给插件读的元数据（<タグ:…>），翻了插件就解析不到；
    //   - $dataSystem.elements：UskElementRate 拿它做
    //     `$dataSystem.elements.indexOf(元素名)` 的字面比较，而事件指令里的
    //     元素名是另写的字面量（elementName:"口"），只改数组这边就断链。
    var DB_TABLES = [
        { name: '$dataItems',        fields: ['name', 'description'] },
        { name: '$dataSkills',       fields: ['name', 'description', 'message1', 'message2'] },
        { name: '$dataWeapons',      fields: ['name', 'description'] },
        { name: '$dataArmors',       fields: ['name', 'description'] },
        { name: '$dataStates',       fields: ['name', 'message1', 'message2', 'message3', 'message4'] },
        { name: '$dataActors',       fields: ['name', 'nickname', 'profile'] },
        { name: '$dataClasses',      fields: ['name'] },
        { name: '$dataEnemies',      fields: ['name'] },
        { name: '$dataTroops',       fields: ['name'] },
        { name: '$dataTilesets',     fields: ['name'] },
        { name: '$dataAnimations',   fields: ['name'] },
        { name: '$dataCommonEvents', fields: ['name'] },
        { name: '$dataMapInfos',     fields: ['name'] }
    ];

    // $dataSystem 不是「记录数组」而是个大对象，另起一组：这几个名字数组在
    // 游戏里会显示（技能/装备类型、存档列表里的标题…），按同样思路整批问、
    // 原地写回同一下标。
    var SYS_ARRAYS = ['switches', 'variables', 'skillTypes', 'equipTypes', 'armorTypes', 'weaponTypes'];
    var SYS_STRINGS = ['gameTitle'];

    // 第一次收齐的原文就固定下来，之后重连还拿它去问。
    // 因为改写是原地做的：再从表里读到的已经是译文，拿译文当 key 问不回来。
    var dbOriginals = null;

    function collectDbTexts() {
        if (dbOriginals) return dbOriginals;

        var seen = Object.create(null);
        var out = [];
        function take(s) {
            if (typeof s !== 'string' || !s || seen[s]) return;
            seen[s] = true;
            out.push(s);
        }

        for (var t = 0; t < DB_TABLES.length; t++) {
            var table = window[DB_TABLES[t].name];
            if (!Array.isArray(table)) continue;
            var fields = DB_TABLES[t].fields;
            // [0] 是空槽（Id 从 1 起），顺手判掉；name 为空的记录不少，跳过。
            for (var i = 0; i < table.length; i++) {
                var rec = table[i];
                if (!rec) continue;
                for (var f = 0; f < fields.length; f++) take(rec[fields[f]]);
            }
        }

        var sys = typeof $dataSystem !== 'undefined' && $dataSystem ? $dataSystem : null;
        if (sys) {
            for (var a = 0; a < SYS_ARRAYS.length; a++) {
                var list = sys[SYS_ARRAYS[a]];
                if (!Array.isArray(list)) continue;
                for (var j = 0; j < list.length; j++) take(list[j]);
            }
            for (var s = 0; s < SYS_STRINGS.length; s++) take(sys[SYS_STRINGS[s]]);
        }

        // 数据库还没载入时别把空结果缓存下来，否则以后再也收不上。
        if (!out.length) return out;
        dbOriginals = out;
        return out;
    }

    function sendDbTexts() {
        var texts = collectDbTexts();
        if (!texts.length) return;   // 数据库还没载入，等 onDatabaseLoaded 再叫一次
        send({ ev: 'terms', loose: true, label: '数据库文本', texts: texts });
        if (debug) console.log(LOG + '数据库文本 ' + texts.length + ' 条，已问 wtool');
    }

    // 这两张表的 `name` 不能改写：这个游戏拿名字当**键**。
    //   - UskEasyScripts.js:346-362 开机时就建了 ES.dataEnemies[name] / ES.dataArmors[name]；
    //   - CommonEvents 的「オーブを使用」更狠，是拿名字当跳板：
    //       v1001 = $gameParty.lastItem().id            （宝珠是 armor）
    //       v1002 = $dataArmors[v1001].name             （名字）
    //       v1003 = ES.dataEnemies[v1002].id            （名字当键）
    // 名字一旦成了中文，v1002 就查不到，报的正是
    // TypeError: Cannot read property 'id' of undefined —— 访问宝珠物品时弹的那个错。
    // 名字不写回也一点译文不丢：画字那一刻走 drawText 钩子，按日文名精确命中词典，
    // 屏幕上照样是中文；而拼进别处的名字会由片段级那一层换掉。
    var NAME_IS_KEY = { $dataArmors: true, $dataEnemies: true };

    // 把译文原地写回各条记录的字段，返回改了几条。
    // 查不到的（词典里没有的）原样留着，保持日文，好过留个空白。
    function applyDbTexts(map) {
        var n = 0;

        for (var t = 0; t < DB_TABLES.length; t++) {
            var table = window[DB_TABLES[t].name];
            if (!Array.isArray(table)) continue;
            var fields = DB_TABLES[t].fields;
            for (var i = 0; i < table.length; i++) {
                var rec = table[i];
                if (!rec) continue;
                for (var f = 0; f < fields.length; f++) {
                    var k = fields[f];
                    if (k === 'name' && NAME_IS_KEY[DB_TABLES[t].name]) continue;
                    if (typeof rec[k] === 'string' && map[rec[k]]) {
                        rec[k] = map[rec[k]];
                        n++;
                    }
                }
            }
        }

        var sys = typeof $dataSystem !== 'undefined' && $dataSystem ? $dataSystem : null;
        if (sys) {
            for (var a = 0; a < SYS_ARRAYS.length; a++) {
                var list = sys[SYS_ARRAYS[a]];
                if (!Array.isArray(list)) continue;
                for (var j = 0; j < list.length; j++) {
                    if (typeof list[j] === 'string' && map[list[j]]) {
                        list[j] = map[list[j]];
                        n++;
                    }
                }
            }
            for (var s = 0; s < SYS_STRINGS.length; s++) {
                var key = SYS_STRINGS[s];
                if (typeof sys[key] === 'string' && map[sys[key]]) {
                    sys[key] = map[sys[key]];
                    n++;
                }
            }
        }

        if (debug) console.log(LOG + '数据库文本已替换 ' + n + ' 条');
        return n;
    }

    //-------------------------------------------------------------------------
    // 事件文本：Map*.json、CommonEvents.json、Troops.json 里的对话、选项、说话人
    //-------------------------------------------------------------------------
    //
    // 对话正文（401）本来就能走「上报 → 回写」那条道，这里补的是它够不着的地方
    // —— 那几处都是渲染时同步取字，等不了一趟往返：
    //   - 选项（102）：Window_ChoiceList 直接从 $gameMessage._choices 取；
    //   - 说话人名（101 的第 5 个参数）：Window_NameBox 读 speakerName()；
    //   - 地图名（Map 文件的 displayName）：进图时 Window_MapName 就画了；
    //   - 战斗事件里的对话（Troops.json）：只有开打才载入，平时问不到。
    //
    // 做法和数据库文本一样：开机把整批原文问过去（loose:true，退级查），
    // 拿回的译文进 dict，上面几处在渲染前从 dict 同步取。原文一个字没动 ——
    // 连地图文件都不读改，只是借它们的原文当 key。
    var NODE = null;
    try {
        NODE = { fs: require('fs'), path: require('path') };
    } catch (e) {
        // 没有 Node 集成（比如拿浏览器跑）就退化成「只翻对话」，
        // 选项和说话人名保持原文，不影响游戏。
    }

    // index.html 所在目录就是 data/ 的家（MZ 是游戏根，MV 在 www/ 下）。
    // 不直接信 process.cwd()：从别处启动时它不是游戏目录。
    function dataDir() {
        if (!NODE) return null;

        var cands = [];
        try {
            var p = decodeURIComponent(window.location.pathname || '');
            if (/^\/[A-Za-z]:/.test(p)) p = p.slice(1);   // /D:/… → D:/…
            cands.push(p.replace(/\/[^\/]*$/, ''));
        } catch (e) {}
        cands.push(process.cwd());
        try {
            cands.push(NODE.path.dirname(process.execPath));
        } catch (e) {}

        for (var i = 0; i < cands.length; i++) {
            if (!cands[i]) continue;
            var d = String(cands[i]).replace(/\\/g, '/') + '/data';
            try {
                if (NODE.fs.existsSync(d)) return d;
            } catch (e) {}
        }
        return null;
    }

    // 把控制码摘掉，剩下的才是真给玩家看的字。
    // 整行只有 \P[1]、\C[14] 这种的，翻了没意义，反而会被片段匹配换出乱码。
    function visible(s) {
        return s.replace(/\\[A-Za-z]*(\[[^\]]*\])?/g, '').trim();
    }

    // 一条事件指令里玩家能看到的文本。10x 这几个码在指令表里是平铺的，
    // 不用递归（分支只靠缩进，不套子数组）。
    function takeCommand(cmd, take) {
        if (!cmd || !cmd.parameters) return;
        var p = cmd.parameters;
        if (cmd.code === 401 || cmd.code === 405) {
            take(p[0]);                                   // 对话正文 / 滚动正文
        } else if (cmd.code === 101) {
            take(p[4]);                                   // 说话人名
        } else if (cmd.code === 102 && Array.isArray(p[0])) {
            for (var i = 0; i < p[0].length; i++) take(p[0][i]);   // 选项
        }
    }

    // 第一次收齐的原文固定下来，之后重连还拿它去问（和 dbOriginals 同理）。
    var evOriginals = null;

    function collectEventTexts() {
        if (evOriginals) return evOriginals;

        // 公共事件和敌群是跟数据库一起载入的。没载入时说明现在问也问不全，
        // 干脆先不缓存，等 onDatabaseLoaded 再叫一次。
        var commons = typeof $dataCommonEvents !== 'undefined' ? $dataCommonEvents : null;
        var troops = typeof $dataTroops !== 'undefined' ? $dataTroops : null;
        if (!Array.isArray(commons) || !Array.isArray(troops)) return [];

        var seen = Object.create(null);
        var out = [];
        // 按 \n 拆开收：对话是逐行查表的（onOpen 里的退级就是逐行走的），
        // key 也得是行，不然一条含换行的 401 会在 dict 里对不上。
        function take(s) {
            if (typeof s !== 'string' || !s) return;
            var parts = s.split('\n');
            for (var i = 0; i < parts.length; i++) {
                var p = parts[i];
                if (!p || seen[p] || !visible(p)) continue;
                seen[p] = true;
                out.push(p);
            }
        }

        // 整段也收一份。一段台词常常连着好几条 401，显示时 onOpen 先把整段
        // join('\n') 查一次 dict，所以词典里得有整段这个 key —— 只收单行的话
        // 整段永远查不中，一段三行的台词就只能「逐行凑」，而行的粒度比段差得
        // 远：实测 1344 段里只有 5 段查不到，却有 183 段是「整段查得到、但其中
        // 有行查不到」，那些行就半日半中地留在屏幕上了。
        //
        // 拼接口径必须和 flatten()+join('\n') 一模一样，否则 key 差一个换行
        // 就白收。
        function takeWhole(s) {
            if (typeof s !== 'string' || s.indexOf('\n') < 0) return;
            if (seen[s] || !visible(s)) return;
            seen[s] = true;
            out.push(s);
        }

        function takeList(list) {
            if (!Array.isArray(list)) return;
            var buf = [];
            function flush() {
                if (!buf.length) return;
                takeWhole(buf.join('\n'));
                buf = [];
            }
            for (var i = 0; i < list.length; i++) {
                var cmd = list[i];
                takeCommand(cmd, take);
                if (cmd && cmd.code === 401 && Array.isArray(cmd.parameters) &&
                    typeof cmd.parameters[0] === 'string') {
                    var parts = cmd.parameters[0].split('\n');
                    for (var j = 0; j < parts.length; j++) buf.push(parts[j]);
                } else {
                    flush();   // 401 断了，一条消息结束
                }
            }
            flush();
        }

        // 地图：data/Map001.json…。180 个文件 5.5 MB，实测 50 ms 上下，
        // 发生在开机载入阶段，看不出来。
        var dir = dataDir();
        if (dir) {
            var files = [];
            try {
                files = NODE.fs.readdirSync(dir).filter(function (f) {
                    return /^Map\d+\.json$/.test(f);
                });
            } catch (e) {
                files = [];
            }
            for (var i = 0; i < files.length; i++) {
                var map;
                try {
                    map = JSON.parse(NODE.fs.readFileSync(dir + '/' + files[i], 'utf8'));
                } catch (e) {
                    continue;   // 单个文件读坏了不该拖垮整批
                }
                take(map.displayName);
                var events = map.events || [];
                for (var e = 0; e < events.length; e++) {
                    var pages = (events[e] || {}).pages || [];
                    for (var p = 0; p < pages.length; p++) takeList((pages[p] || {}).list);
                }
            }
        }

        for (var c = 0; c < commons.length; c++) takeList((commons[c] || {}).list);
        for (var t = 0; t < troops.length; t++) {
            var tpages = (troops[t] || {}).pages || [];
            for (var q = 0; q < tpages.length; q++) takeList((tpages[q] || {}).list);
        }

        if (!out.length) return out;
        evOriginals = out;
        return out;
    }

    function sendEventTexts() {
        var texts = collectEventTexts();
        if (!texts.length) return;
        send({ ev: 'terms', loose: true, label: '事件文本', texts: texts });
        if (debug) console.log(LOG + '事件文本 ' + texts.length + ' 条，已问 wtool');
    }

    // 选项：Window_ChoiceList 是从 $gameMessage._choices 取字画的，在它
    // refresh 之前换掉就行。402 是按下标分支的，换字不影响流程。
    //
    // 和对话正文同理，选项里也可能写 \v[n]（实测这个游戏就有
    // `\i[127]略奪 \c[2][自衛力:\v[373]]` 这种），先展开再查表。
    function replaceChoices() {
        var choices = $gameMessage._choices;
        if (!Array.isArray(choices)) return;
        for (var i = 0; i < choices.length; i++) {
            if (typeof choices[i] !== 'string') continue;
            var hit = dict[expandVariables(choices[i])];
            if (hit) choices[i] = hit;
        }
    }

    var _choiceStart = Window_ChoiceList.prototype.start;
    Window_ChoiceList.prototype.start = function () {
        replaceChoices();
        return _choiceStart.apply(this, arguments);
    };

    // 地图名：进图时 Window_MapName 直接读 $gameMap.displayName()，
    // 中间没有能拦的地方，只能在数据落地时改写。displayName 不参与任何
    // 逻辑比较，改它没有副作用。
    var _dataOnLoad = DataManager.onLoad;
    DataManager.onLoad = function (object) {
        // 刻意不调 DataManager.isMapObject：那是 MZ 才有的，MV 上它是 undefined，
        // 一调就 `is not a function`，游戏直接弹错误框。改按「谁带 displayName
        // 字符串谁就是地图」判 —— 数据文件里只有地图对象有这个字段。
        if (object && typeof object.displayName === 'string') {
            if (dict[object.displayName]) object.displayName = dict[object.displayName];
        }
        return _dataOnLoad.apply(this, arguments);
    };

    //-------------------------------------------------------------------------
    // 窗口文本：菜单、列表这类「整串画」的地方
    //-------------------------------------------------------------------------
    //
    // 对话是逐字画的（Window_Message 一帧一个字地 processCharacter），所以那边
    // 只能挂在 startMessage 上；菜单不一样 —— 它调 drawTextEx/drawText 时手里
    // 就是**完整的一串**，这正是查表要的粒度。
    //
    // 为什么以前翻不到：本作的自定义菜单（SceneCustomMenu）把菜单项文字写在
    // **插件参数**里（js/plugins.js 的 SceneCustomMenu.Scene1 → "\I[188]手帳"），
    // 既不进 $dataSystem.terms，也不进那四张数据库表，前面几条「整批问」的管道
    // 一条都碰不到它，所以屏幕上一直是日文。而 MTool 把整本词典灌进游戏进程，
    // 是画字那一刻同步查的，自然不受这个限制。
    //
    // 这里补上同一条路：画之前先查 dict，dict 里没有就当场同步问 wtool 一句
    // （见 syncLookup，走的是 `engine::lookup` 那条专用通道），问着了这一遍就是
    // 中文，不用重画、也不挑窗口有没有 refresh()。
    // 同步这条道问不着（wtool 没起、词典里真没有、超时熔断）才退回老路：先记下来
    // 攒一批问（走 loose，让 wtool 那边退级去啃整串里的片段），等回话到了重画一遍 ——
    // 那条路第一遍是日文、第二遍起才是中文。
    //
    // 只挂 Window_Base 的 drawText/drawTextEx，**不挂 Bitmap.drawText**：逐字绘制
    // 最终落在 contents.drawText（Bitmap 上的），那会儿手里只有一个字符，拿单字
    // 查表正是之前「どこに行くの？→ どこ中行くの？」那类文本错位的成因。这里拿到的
    // 永远是整串，查不到原样放行，啃不到单字。

    // 已经问过 wtool 的串（含没翻上的）。重画很频繁，不能每次都重问。
    var askedDrawn = Object.create(null);
    // 攒着待问的。画字每帧都可能发生，一条一发会把 socket 打满。
    var pendingDrawn = Object.create(null);
    var drainTimer = 0;

    // 值不值得问。得至少有假名或表意字 —— 纯 ASCII 的数字、伤害值、存档槽位
    // 问了也是白问；而词典里那些真译文要挡掉，否则中文被当成原文再翻一遍，
    // wtool 的片段匹配能把它改出鬼来（实测抽样 2.7%）。译文都进过 translated 名单。
    function worthAsking(s) {
        if (s.length < 2) return false;
        if (translated[s] || dict[s]) return false;
        return /[\u3040-\u30ff\u3400-\u9fff\uff66-\uff9f]/.test(s);
    }

    function learnDrawn(text) {
        if (!socket || socket.destroyed) return;   // 还没连上，问了也没人接
        if (askedDrawn[text] || !worthAsking(text)) return;
        askedDrawn[text] = true;
        pendingDrawn[text] = true;
        if (!drainTimer) drainTimer = setTimeout(drainDrawn, 50);
    }

    function drainDrawn() {
        drainTimer = 0;
        var texts = [];
        for (var k in pendingDrawn) texts.push(k);
        pendingDrawn = Object.create(null);
        if (!texts.length) return;
        send({ ev: 'terms', loose: true, label: '窗口文本', texts: texts });
        if (debug) console.log(LOG + '窗口文本 ' + texts.length + ' 条，已问 wtool');
    }

    // 画之前把整串换掉。dict 里有就换，没有就**当场同步问一次**（见 syncLookup），
    // 问着了直接就是译文 —— 不会先画一遍日文再重画，也不挑窗口有没有 refresh()。
    // 问不着（wtool 没起、词典里真没有、超时、熔断）才退回攒批那条老路。
    function translateDrawn(text) {
        if (typeof text !== 'string' || !text) return text;
        var hit = dict[text];
        if (hit) return hit;
        if (worthAsking(text)) {
            hit = syncLookup(text);
            if (hit) {
                // 记进 dict，同一串之后就直接命中，不再走通道；译文也记进
                // translated —— worthAsking 靠它挡住「拿中文再翻一遍」。
                dict[text] = hit;
                translated[hit] = true;
                return hit;
            }
        }
        learnDrawn(text);
        return text;
    }

    // 回话到了，让当前画面上那些窗口重画一遍 —— 否则已经画上去的日文不会自己变。
    // 跳过消息窗口：它有自己那套（applyTranslation → repaint），在这里重画会把
    // 正逐字打着的那句从头再打一遍。
    function refreshWindows() {
        var scene = typeof SceneManager !== 'undefined' ? SceneManager._scene : null;
        var layer = scene ? scene._windowLayer : null;
        if (!layer || !layer.children) return;
        var msg = scene._messageWindow;
        for (var i = 0; i < layer.children.length; i++) {
            var w = layer.children[i];
            if (w === msg || !w || typeof w.refresh !== 'function') continue;
            try {
                w.refresh();
            } catch (e) {
                // 单个窗口刷新失败不该拖垮别的。
            }
        }
    }

    var _winDrawText = Window_Base.prototype.drawText;
    Window_Base.prototype.drawText = function (text, x, y, maxWidth, align) {
        return _winDrawText.call(this, translateDrawn(text), x, y, maxWidth, align);
    };

    var _winDrawTextEx = Window_Base.prototype.drawTextEx;
    Window_Base.prototype.drawTextEx = function (text, x, y, width) {
        return _winDrawTextEx.call(this, translateDrawn(text), x, y, width);
    };

    //-------------------------------------------------------------------------
    // 展开之后的文本：`\tx[...]`、`\js[...]` 写出来的日文
    //-------------------------------------------------------------------------
    //
    // 上面那两个钩子拿到的是**控制码串**，比如技能 290 的说明栏里写的是
    //   `\js[sin_drain]\n\js[sin_drain2]`
    // 这东西词典里当然没有（MTool 的 TrsData.bin 也没有）。展开发生在
    //   Window_Base.createTextState → convertEscapeCharacters
    //     → PluginCommonBase 覆写的那一层 → TextScriptBase 的 /\x1bJS\[(.+?)]/ 替换
    // 也就是**在上面两个钩子更靠里的地方**：展开出来的日文（这句就是
    // 「相手の命を奪い尽くし我が物とする悪魔の業」）再被逐块画出去，从来不经我们的手，
    // 所以这条技能说明一直是日文。这里在展开之后再看一眼 —— 展开出来的才是玩家眼里的原文。
    //
    // 查表要按行来：展开结果可能多行（一句一行），整串查不中，逐行才有粒度。
    // 查出来的键用的是原文写法（`\C[2]` 这种），而这一层的文本已经过了
    // convertEscapeCharacters（`\` 都成了 \x1b 0x1b，`\\` 收成了 `\`），所以
    // 查之前换回去、写回之后再换过来（toTextState）—— 这都是引擎自己那两条规则的逆。
    function toLookupKey(s) {
        return s.indexOf('\x1b') < 0 ? s : s.replace(/\x1b/g, '\\');
    }

    function toTextState(s) {
        if (s.indexOf('\\') < 0) return s;
        return s.replace(/\\\\/g, '\u0000').replace(/\\/g, '\x1b').replace(/\u0000/g, '\\');
    }

    function translateExpanded(text) {
        var lines = text.split('\n');
        var changed = false;
        for (var i = 0; i < lines.length; i++) {
            if (!lines[i]) continue;
            var key = toLookupKey(lines[i]);
            // 走 translateDrawn 而不是直接查：命中会记进 dict，问过的会进 askedDrawn，
            // 同一行重画多少次都不会再问一遍（问不着的那种是 50ms 一次的代价）。
            var hit = translateDrawn(key);
            if (hit === key) continue;
            lines[i] = toTextState(hit);
            changed = true;
        }
        return changed ? lines.join('\n') : text;
    }

    var _createTextState = Window_Base.prototype.createTextState;
    Window_Base.prototype.createTextState = function (text, x, y, width) {
        var st = _createTextState.apply(this, arguments);
        // calcTextHeight 已经按原文算过了：这里只换行内容、不动行数，高度不受影响。
        if (st && typeof st.text === 'string' && st.text) {
            var t = translateExpanded(st.text);
            if (t !== st.text) st.text = t;
        }
        return st;
    };

    // 这两张表都是数据库载入时才有的（$dataSystem 和 $dataItems 一起载入），
    // 这是唯一能保证它们已经存在的时机。但那一刻 socket 未必连上了，
    // 所以两头都要叫：这里叫一次、connect 那次也叫一次，谁先到就谁发，重了也无害。
    var bootSent = false;

    function sendBootTexts() {
        // 数据库还没载入就什么都不做，也别置标记 —— 下一帧还会再叫。
        if (typeof $dataSystem === 'undefined' || !$dataSystem) return;
        if (bootSent) return;
        bootSent = true;
        sendTerms();
        sendDbTexts();
        sendEventTexts();
    }

    // MZ 走这条。MV 上没有 onDatabaseLoaded 这个函数，跳过。
    if (typeof Scene_Boot.prototype.onDatabaseLoaded === 'function') {
        var _onDatabaseLoaded = Scene_Boot.prototype.onDatabaseLoaded;
        Scene_Boot.prototype.onDatabaseLoaded = function () {
            _onDatabaseLoaded.apply(this, arguments);
            sendBootTexts();
        };
    }

    // MV 走这条：isReady 转 true 的那一刻数据库已载入。MZ 上也有 isReady，
    // 两边都挂，谁先到谁发，bootSent 保证只发一次。
    var _bootIsReady = Scene_Boot.prototype.isReady;
    Scene_Boot.prototype.isReady = function () {
        var ready = _bootIsReady.apply(this, arguments);
        if (ready) sendBootTexts();
        return ready;
    };

    //-------------------------------------------------------------------------
    // 独特数据：data/unique/ 下那两张表（$DU）
    //-------------------------------------------------------------------------
    //
    // UniqueDataLoader 把 SuccubusData.json 挂到 $DU.SD、HeroineTalk.json 挂到
    // $DU.HeroineTalk，全载完在 Scene_Boot.prototype.onUniqueDataLoad 打个招呼
    // —— 这是它们唯一的就绪信号（UniqueDataLoader.js:156 那个空函数）。
    //
    // 这两张表跟前几批不一样：它们在运行时被读，而且有一条绕路
    //   - $DU.HeroineTalk：ES.getHeroinTalk() 直接读（UskEasyScripts.js:236），
    //     事件指令 7 号公共事件用变量 1001-1005 中转。
    //   - $DU.SD：事件指令先把它**整条对象**赋给变量 25（$gameVariables.setValue
    //     存的是引用，不是副本），再由 ES.getSD/getSDM/getSDT 从变量里取
    //     （UskEasyScripts.js:94/99/158）。UskEncounterEffect 和 UskEnemyStatusScene
    //     另有两处直读 .name/.profile/.cv/.illust。
    // 引用也好、直读也好，都指向同一个对象，所以「原地把值改成译文」就一步到位：
    // 不用挂钩子，也不用等什么时机，磁盘上的 json 一个字节没动。
    //
    // 只收给人看的字，逻辑枚举一个不碰 —— 翻了它游戏就找不到分支了：
    //   monsterType（"devil"/"beast"…，ES.getSD 的字面比较）
    //   SPAction / criticalAction（动作名数组）
    //   talk.*.actionNo、talk.*.requestPleasureAction、talk.normal[].pattern[].action
    // HeroineTalk 的**键**是查询用的 flag（"リリス：出口"），getHeroinTalk 拿它
    // 找行、completeHeroinTalk 拿它做 indexOf 匹配，同样不能动，只翻值。
    var SD_SKIP = {
        monsterType: 1,
        SPAction: 1,
        criticalAction: 1,
        action: 1,
        actionNo: 1,
        requestPleasureAction: 1
    };

    // 第一次收齐的原文固定下来，之后重连还拿它去问（和 dbOriginals 同理）。
    var duOriginals = null;

    function collectDuTexts() {
        if (duOriginals) return duOriginals;

        var DU = typeof $DU !== 'undefined' ? $DU : null;
        if (!DU) return [];

        var seen = Object.create(null);
        var out = [];
        // 按 \n 拆开收：消息窗口是逐行查表的，key 也得是行。
        // 整行只有 \P[1]、\C[14] 这种控制码的（说话人栏里就有）不算文本。
        function take(s) {
            if (typeof s !== 'string' || !s) return;
            var parts = s.split('\n');
            for (var i = 0; i < parts.length; i++) {
                var p = parts[i];
                if (!p || seen[p] || !visible(p)) continue;
                seen[p] = true;
                out.push(p);
            }
        }
        // SD 的字段形状不齐：profile 是数组、message 有的套两层有的套三层
        // （message.P_resist.normal / message.etc.オーダー / message.miss[]），
        // 与其按字段列表枚举，不如整棵走一遍，把该跳的键名挑出去。
        function walk(v) {
            if (typeof v === 'string') {
                take(v);
            } else if (Array.isArray(v)) {
                for (var i = 0; i < v.length; i++) walk(v[i]);
            } else if (v && typeof v === 'object') {
                for (var k in v) {
                    if (SD_SKIP[k]) continue;
                    walk(v[k]);
                }
            }
        }

        var sd = DU.SD;
        if (sd && typeof sd === 'object') {
            for (var key in sd) walk(sd[key]);
        }

        // HeroineTalk：每个 flag 下是一串行，形状就三种（实测 34 个 flag、223 行）
        //   ["リリス", "正文", 0]      说话人 + 正文
        //   ["select", "选项1", "选项2"] 选项
        //   ["balloon", -1, 6]         只有数字，没字
        var ht = DU.HeroineTalk;
        if (ht && typeof ht === 'object') {
            for (var flag in ht) {
                var rows = ht[flag];
                if (!Array.isArray(rows)) continue;
                for (var r = 0; r < rows.length; r++) {
                    var row = rows[r];
                    if (!Array.isArray(row)) continue;
                    if (row[0] === 'balloon') continue;
                    if (row[0] === 'select') {
                        for (var c = 1; c < row.length; c++) take(row[c]);
                        continue;
                    }
                    take(row[0]);   // 说话人名（\P[1] 那种会被 visible 挡掉）
                    take(row[1]);   // 正文
                }
            }
        }

        // 没载入时别把空结果缓存下来，否则以后再也收不上。
        if (!out.length) return out;
        duOriginals = out;
        return out;
    }

    function sendDuTexts() {
        var texts = collectDuTexts();
        if (!texts.length) return;   // $DU 还没载入，等 onUniqueDataLoad 再叫一次
        send({ ev: 'terms', loose: true, label: '独特数据', texts: texts });
        if (debug) console.log(LOG + '独特数据 ' + texts.length + ' 条，已问 wtool');
    }

    // 把译文原地写回 SD 与 HeroineTalk。和上面几批一样，查不到的原样留着。
    // 注意是「就地改字段/改行」，不是换掉整个对象：$-变量里存的是同一个引用。
    function applyDuTexts(map) {
        var DU = typeof $DU !== 'undefined' ? $DU : null;
        if (!DU) return 0;

        var n = 0;
        function walk(v) {
            if (Array.isArray(v)) {
                for (var i = 0; i < v.length; i++) {
                    if (v[i] && typeof v[i] === 'object') walk(v[i]);
                    else if (typeof v[i] === 'string' && map[v[i]]) { v[i] = map[v[i]]; n++; }
                }
            } else if (v && typeof v === 'object') {
                for (var k in v) {
                    if (SD_SKIP[k]) continue;
                    var t = v[k];
                    if (t && typeof t === 'object') walk(t);
                    else if (typeof t === 'string' && map[t]) { v[k] = map[t]; n++; }
                }
            }
        }

        var sd = DU.SD;
        if (sd && typeof sd === 'object') {
            for (var key in sd) walk(sd[key]);
        }

        // HeroineTalk 是按行、按下标改的，得挑着来：翻 row[0] 会把 "select"、
        // "balloon" 这种行头也换掉，后面的 switch 就认不出来了。
        var ht = DU.HeroineTalk;
        if (ht && typeof ht === 'object') {
            for (var flag in ht) {
                var rows = ht[flag];
                if (!Array.isArray(rows)) continue;
                for (var r = 0; r < rows.length; r++) {
                    var row = rows[r];
                    if (!Array.isArray(row)) continue;
                    if (row[0] === 'balloon') continue;
                    if (row[0] === 'select') {
                        for (var c = 1; c < row.length; c++) {
                            if (typeof row[c] === 'string' && map[row[c]]) { row[c] = map[row[c]]; n++; }
                        }
                        continue;
                    }
                    if (typeof row[0] === 'string' && map[row[0]]) { row[0] = map[row[0]]; n++; }
                    if (typeof row[1] === 'string' && map[row[1]]) { row[1] = map[row[1]]; n++; }
                }
            }
        }

        if (debug) console.log(LOG + '独特数据已替换 ' + n + ' 条');
        return n;
    }

    // 和 onDatabaseLoaded 同理：onUniqueDataLoad 是 $DU 就绪的唯一信号，
    // 那一刻 socket 未必连上，所以两头都叫一次，谁先到谁发。
    // 这个钩子是游戏自带的 UniqueDataLoader 装上的，不是引擎 API，所以先确认
    // 它真的在（没装这个插件的游戏就没有 $DU，也就没有这个钩子）。
    if (typeof Scene_Boot.prototype.onUniqueDataLoad === 'function') {
        var _onUniqueDataLoad = Scene_Boot.prototype.onUniqueDataLoad;
        Scene_Boot.prototype.onUniqueDataLoad = function () {
            _onUniqueDataLoad.apply(this, arguments);
            sendDuTexts();
        };
    }

    //-------------------------------------------------------------------------

    window.wtool = {
        current: function () {
            return { active: state.active, seq: state.seq, lines: state.lines.slice(), text: state.text };
        },
        dictSize: function () {
            return Object.keys(dict).length;
        },
        clearDict: function () {
            dict = Object.create(null);
        },
        setDebug: function (on) {
            debug = !!on;
        },
        resendAll: function () {
            sendTerms();
            sendDbTexts();
            sendEventTexts();
            sendDuTexts();
        },
        gameMessage: function () {
            return $gameMessage;
        }
    };

    connect();
})();
