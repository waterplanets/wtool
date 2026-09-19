//=============================================================================
// wtool_reader.js
//=============================================================================
/*:
 * @plugindesc 实时读取当前对话框文本（观测用，不替换任何文本）
 * @author wtool
 *
 * @help
 * 目的：先确认能实时拿到 MV 的对话文本，再谈翻译。
 *
 * 安装：
 *   1. 本文件放进 游戏目录/www/js/plugins/
 *   2. 打开 游戏目录/www/js/plugins.js，在数组里加一行：
 *        {"name":"wtool_reader","status":true,"parameters":{}}
 *   3. 启动游戏
 *
 * 取值：
 *   wtool.current()       当前对话框快照，可直接 JSON 序列化
 *                         { active, seq, lines, text }
 *   wtool.onMessage(fn)   订阅 open / close，返回取消订阅函数
 *   wtool.setDebug(false) 关闭控制台输出
 *   wtool.gameMessage()   拿原始的 $gameMessage，想自己看别的字段时用
 *
 * 说明：
 *   - 读到的 text 是「原文」，\N[1] / \V[10] 这类转义码尚未转换。
 *     词典的 key 就应该是这个形态，转换是渲染时才发生的事。
 *   - 这是探测器，不替换任何文本，随时可安全移除。
 */
(function () {
    'use strict';

    var LOG_PREFIX = '[wtool] ';
    var debug = true;

    // 当前对话框状态。emit 出去的和 current() 返回的都是这个形状的副本。
    var state = { active: false, seq: 0, lines: [], text: '' };
    var listeners = [];

    function flatten(texts) {
        // $gameMessage._texts 是「事件指令 401 的参数」数组，
        // 每一条自身还可能含 \n，这里摊平成实际显示的行。
        var out = [];
        for (var i = 0; i < texts.length; i++) {
            var parts = String(texts[i]).split('\n');
            for (var j = 0; j < parts.length; j++) {
                out.push(parts[j]);
            }
        }
        return out;
    }

    function snapshot() {
        return {
            active: state.active,
            seq: state.seq,
            lines: state.lines.slice(),
            text: state.text
        };
    }

    function emit(event, snap) {
        for (var i = 0; i < listeners.length; i++) {
            try {
                listeners[i](event, snap);
            } catch (e) {
                // 订阅方出错不能影响游戏
                console.error(LOG_PREFIX + '订阅回调抛错', e);
            }
        }
    }

    function onOpen() {
        var lines = flatten($gameMessage._texts || []);
        state = {
            active: true,
            seq: state.seq + 1,
            lines: lines,
            text: lines.join('\n')
        };
        if (debug) {
            console.log(LOG_PREFIX + '对话框打开 #' + state.seq);
            console.log(state.text);
        }
        emit('open', snapshot());
    }

    function onClose() {
        if (!state.active) return;
        state = { active: false, seq: state.seq, lines: [], text: '' };
        if (debug) console.log(LOG_PREFIX + '对话框关闭');
        emit('close', snapshot());
    }

    // 对话是逐字画的，不能挂 drawText；startMessage 里 _texts 才是完整的整句。
    var _startMessage = Window_Message.prototype.startMessage;
    Window_Message.prototype.startMessage = function () {
        onOpen();
        return _startMessage.call(this);
    };

    var _terminateMessage = Window_Message.prototype.terminateMessage;
    Window_Message.prototype.terminateMessage = function () {
        onClose();
        return _terminateMessage.call(this);
    };

    // 挂在 window 上，控制台里可直接调；将来走 CDP 也是 Runtime.evaluate 直接取。
    window.wtool = {
        current: snapshot,
        onMessage: function (fn) {
            listeners.push(fn);
            return function () {
                var i = listeners.indexOf(fn);
                if (i >= 0) listeners.splice(i, 1);
            };
        },
        setDebug: function (on) {
            debug = !!on;
        },
        gameMessage: function () {
            return $gameMessage;
        }
    };
})();
