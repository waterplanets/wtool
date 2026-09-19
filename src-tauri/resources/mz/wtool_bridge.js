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
 *   - 某段对话「第一次」出现时，wtool 还没把译文送回来，画出来的仍是原文；
 *     等回写到达后本插件会让消息窗口重画一次，视觉上可能闪一下。
 *     之后同样的文本再出现，就会在画之前同步替换，不再闪。
 *     （整批问回来的那批文本不存在这个问题：dict 里早就有译文了。）
 *   - 上报给 wtool 的 text 永远是原文，写进游戏的才是译文，
 *     所以来回查表用的 key 不会互相污染。
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

    function onOpen() {
        var lines = flatten($gameMessage._texts || []);
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
                if (dict[out[i]]) {
                    out[i] = dict[out[i]];
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
    function replaceChoices() {
        var choices = $gameMessage._choices;
        if (!Array.isArray(choices)) return;
        for (var i = 0; i < choices.length; i++) {
            if (typeof choices[i] === 'string' && dict[choices[i]]) {
                choices[i] = dict[choices[i]];
            }
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
        if (DataManager.isMapObject(object) && typeof object.displayName === 'string') {
            if (dict[object.displayName]) object.displayName = dict[object.displayName];
        }
        return _dataOnLoad.apply(this, arguments);
    };

    // 这两张表都是数据库载入时才有的（$dataSystem 和 $dataItems 一起载入），
    // 这是唯一能保证它们已经存在的时机。但那一刻 socket 未必连上了，
    // 所以两头都要叫：这里叫一次、connect 那次也叫一次，谁先到就谁发，重了也无害。
    var _onDatabaseLoaded = Scene_Boot.prototype.onDatabaseLoaded;
    Scene_Boot.prototype.onDatabaseLoaded = function () {
        _onDatabaseLoaded.apply(this, arguments);
        sendTerms();
        sendDbTexts();
        sendEventTexts();
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
    var _onUniqueDataLoad = Scene_Boot.prototype.onUniqueDataLoad;
    Scene_Boot.prototype.onUniqueDataLoad = function () {
        _onUniqueDataLoad.apply(this, arguments);
        sendDuTexts();
    };

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
