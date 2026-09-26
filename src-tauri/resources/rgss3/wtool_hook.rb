# encoding: utf-8
#==============================================================================
# wtool RGSS3 (VX Ace) 实时翻译 hook
#------------------------------------------------------------------------------
# 由 Patch/Patch.rb 末尾的 loader 用 eval 载入（NWPatch 机制，零注入）。
#
# 这一份源码同时服务两条注入路，差别只在下面那几行配置（由 wtool 安装时改写）：
#   一次性：LIVE=false，词典烤成 Patch/wtool_dict.tsv，装完即终态。
#   实时：  LIVE=true，  wtool 改写词典/字体文件，钩子每秒比一次 mtime 热重载。
#
# 卸载：见 src-tauri/src/engine/rgss3/{patch,live}.rs —— 删掉对应文件并把
#       Patch/Patch.rb 用 Patch/Patch.rb.wtool-bak 覆盖回去（或就地摘掉 loader）。
#
# 挂载策略：全类扫描（照抄 MTool，见 .probe_rgss3/mtool_scripts/evalIBin_173.rb）
#------------------------------------------------------------------------------
# 最初我们只挂 Window_Base / Window_Message / Bitmap 三个固定位置，结果是
# 「场景回忆、图鉴字段、存档读档」整片漏翻：这些界面各自定义了窗口类，并且
# **自己 override 了 draw_text 等方法** —— 子类一 override，基类上的钩子就
# 整条失效。
#
# MTool 的做法是 `ObjectSpace.each_object(Class)` 遍历游戏里所有已加载的类，
# 谁定义了就单独给谁挂一份。下面是同一套策略，挂载点：
#
#   draw_text              全参数扫描（签名有 rect 版和 5 参数版两种，扫全部更稳）
#   draw_text_ex           args[2]
#   text_size              args[0]（译文长度变了要同步算，否则排版错位）
#   calc_line_width        args[0]
#   calc_line_height       args[0]
#   set_text               args[0]（不少自定义窗口用这个塞文本）
#   draw_autoreturn_text   全参数扫描
#   start_message          把 $game_message.texts 换成译文（对话入口，不碰绘制逻辑）
#
# 另外两个不进通用扫描，单独处理：
#   Window_Message#process_all_text  对话逐字绘制，要改写绘制逻辑（见下）
#   Bitmap#draw_text                 逐字绘制的底层出口，MTool 也刻意跳过它；
#                                    这里保留守卫版兜底（见 install 末尾）
#
# 字体：换掉游戏自带的日文字体链（见 install_font）—— 日文字体画简体中文会
#       缺字形、而且汉字是日式写法（「直」「画」「骨」这些和简体不同）。
#==============================================================================

module WTool
  VERSION = "9"

  # ---- 安装时可替换的配置（wtool 按模式改写下面这几行）------------------------
  #
  # 一次性模式：LIVE=false。词典在安装时就转好烤在盘上，钩子开机读一次就完事，
  #            wtool 关掉也照翻（装完即终态）。
  # 实时模式：  LIVE=true。wtool 开着，把词典和字体写进这几个文件；钩子每秒比
  #            一次 mtime，谁变了就地重载 —— **换字体不用重装**，这就是实时
  #            模式存在的意义（调字体、试词典）。
  #
  # 注意：这几行的**字面形状**（等号两边的对齐空格）Rust 侧是按精确子串替换的，
  # 改这里必须同步改 src-tauri/src/engine/rgss3/patch.rs 里对应的常量
  # （LIVE_LINE / DICT_LINE / LOG_LINE / MISS_LINE / ERR_LINE / FONT_PATH_LINE）。
  LIVE      = false
  DICT_PATH = "Patch/wtool_dict.tsv"
  LOG_PATH  = "Patch/wtool_hook.log"
  MISS_PATH = "Patch/wtool_miss.log"
  ERR_PATH  = "Patch/wtool_hook.err"
  FONT_PATH = ""
  # ---------------------------------------------------------------------------

  # 「该翻但没翻出来」的原文落盘上限。这些串是照着游戏画面抄的，用户没法
  # 一条条打给我，所以让钩子自己记 —— 之后据此补词典。
  MISS_MAX  = 3000
  # 片段回退的告警上限。片段匹配是「整段没命中」之后的兜底，比整段匹配容易
  # 出错，所以每替换一次就记一条，方便事后核对有没有翻坏。
  FRAG_LOG_MAX = 500

  # 片段匹配的窗口上限**不写死**：载词典时顺手记下最长 key（@max_frag），拿它当
  # 上限。
  #
  # 曾经写死 30，代价是长 key 整条够不着 —— 社区词典是按消息窗宽度切过句的，
  # 键最长 44 字，超过 30 的有 7629 条。够不着长键，贪心匹配就退到 `激しく`
  # `れる` 这种碎键，把译文拼进单词中间（详见 `lookup_frag`）。

  # 片段匹配结果的缓存上限。UI 串重复度极高（每帧都会重画同一批标签），
  # 缓存能把这层开销摊成一次性。
  CACHE_MAX = 20000

  # 含假名或汉字才算「可能要翻」。纯 ASCII / 纯符号直接跳过，省掉查表。
  NEEDS_TR  = /[ぁ-んァ-ヶー一-龠々〇]/
  # 假名（含长音符）。Bitmap 兜底层用它判断「这条要不要试着翻一下」。
  KANA      = /[ぁ-ゖァ-ヺー]/
  # 已完成转义的控制码（convert_escape_characters 之后的形式）：\eC[2] 等
  ESC_CODE  = /\e[A-Za-z]\[[^\]]*\]/
  # 未转义的控制码（原文里的形式）：\c[2] \i[5] \v[10] 等
  RAW_CODE  = /\\[A-Za-z]+\[[^\]]*\]/

  # 渲染中文用的字体。日文游戏自带的字体链（MS Gothic / UmePlus 之类）拿来画
  # 简体中文有两个毛病：一部分简体字根本没有字形（画成豆腐块），汉字字形也是
  # 日式的（「直」「画」「骨」「每」这些和简体写法不同）。所以要换成中文字体。
  #
  # MTool 也是换成「黑体」（它下发的脚本里写死 $rgssFontName = "黑体"）。
  # 想换别的字体就改这一行，Windows 自带的还有「微软雅黑」。
  FONT_NAME = "黑体"

  # 全类扫描时尝试挂载的方法 → 「文本在第几个参数上」。:all = 全参数扫描。
  # 这里故意不含 process_all_text / draw_text（Bitmap 专用）—— 那两个单独挂。
  TEXT_ARG = {
    :draw_text            => :all,
    :draw_text_ex         => 2,
    :text_size            => 0,
    :calc_line_width      => 0,
    :calc_line_height     => 0,
    :set_text             => 0,
    :draw_autoreturn_text => :all,
  }
  HOOK_METHODS = TEXT_ARG.keys

  @dict  = {}
  @frag  = {}               # 片段匹配结果缓存（原文 => 译文 / nil）
  @max_frag = 2             # 词典里最长的 key 有几个字；片段匹配的上限就是它
  @miss  = {}               # 已记过的未命中原文（去重，免得每帧重画刷爆日志）
  @trans = {}               # 已经产出过的译文（防多层钩子把译文再翻一遍）
  @frag_warn = 0            # 已写出的片段回退告警条数（见 FRAG_LOG_MAX）
  @hits  = 0
  @sample_limit = 30        # 前 30 次命中写进日志，用来确认 hook 真的在工作
  # ---- 实时模式（LIVE=true）用的状态 ----
  @font_name = nil          # 从 FONT_PATH 读到的字体名；nil = 用源码里的 FONT_NAME
  @dict_mtime = nil         # 词典文件的 mtime，用来判断要不要重载
  @font_mtime = nil         # 字体文件的 mtime
  @last_tick = 0            # 上次查文件的时刻（限流到每秒一次）

  # 当前生效的字体名：字体文件优先，其次才是烤进源码的 FONT_NAME。
  def self.font_name
    n = @font_name
    return n if n.is_a?(String) && !n.empty?
    FONT_NAME
  end

  # 实时模式：挂 Graphics.update，每秒比一次词典/字体文件的 mtime。
  # 一次性模式（LIVE=false）根本不装这个，钩子读完一次就完事。
  def self.file_mtime(path)
    return nil if path.nil? || path.empty?
    File.exist?(path) ? File.mtime(path).to_f : nil
  rescue Exception
    nil
  end

  # 读字体文件并热更新。空内容 = 回退到默认字体（这样「清空字体」也传得进来）。
  def self.read_font_file
    return if FONT_PATH.nil? || FONT_PATH.empty?
    return unless File.exist?(FONT_PATH)
    name = File.open(FONT_PATH, "r:utf-8") { |f| f.read }.strip
    name = FONT_NAME if name.empty?
    return if name == @font_name
    @font_name = name
    apply_font_name
    log "字体热更新为「#{name}」"
  rescue Exception => e
    log "字体热更新失败：#{e.class}: #{e.message}"
  end

  # 把当前字体设成全局默认：之后新建的 Font 都会用它。
  # 不碰已有 Font 对象（那要等窗口重建），但界面标签、菜单大多会重建。
  def self.apply_font_name
    return unless defined?(Font)
    n = font_name
    return if n.nil? || n.empty?
    begin
      Font.default_name = n
    rescue Exception
    end
  end

  # 词典或字体文件变了就地重载。载词典前把片段缓存、译文缓存、最长键一并复位
  #（词典换了，之前算出来的最长键和缓存都不作数了）。
  def self.check_reload
    if file_mtime(DICT_PATH) != @dict_mtime
      @frag.clear
      @trans.clear
      @max_frag = 2
      log "词典变化，重新载入……"
      load_dict
    end
    if file_mtime(FONT_PATH) != @font_mtime
      @font_mtime = file_mtime(FONT_PATH)
      read_font_file
    end
  rescue Exception
    nil
  end

  # Graphics.update 每秒会被调很多次，这里限流到一秒一次，免得每帧都去 stat。
  def self.tick
    now = Time.now.to_f
    return if now - @last_tick < 1.0
    @last_tick = now
    check_reload
  end

  # 实时模式才装：把 tick 挂到 Graphics.update 上。
  def self.install_tick
    unless defined?(Graphics) && Graphics.respond_to?(:update)
      log "警告：Graphics.update 不可用，实时热重载未启用"
      return
    end
    class << Graphics
      unless method_defined?(:wtool_orig_update)
        alias_method :wtool_orig_update, :update
        def update(*args, &blk)
          WTool.tick
          wtool_orig_update(*args, &blk)
        end
      end
    end
    log "实时热重载已启用（每秒查一次词典/字体）"
  end

  def self.log(msg)
    File.open(LOG_PATH, "a:utf-8") do |f|
      f.puts("[#{Time.now.strftime('%H:%M:%S')}] #{msg}")
    end
  rescue Exception
    # 日志写不进去（目录不存在/被占用）不该影响游戏
  end

  #--------------------------------------------------------------------------
  # ● 载入词典
  #--------------------------------------------------------------------------
  def self.load_dict
    # 先记下 mtime：即使词典不存在/读失败也要记，否则实时模式下会每秒重试一遍。
    @dict_mtime = file_mtime(DICT_PATH)
    unless File.exist?(DICT_PATH)
      log "词典不存在，跳过：#{DICT_PATH}"
      return false
    end

    t0 = Time.now
    File.open(DICT_PATH, "r:utf-8") do |f|
      f.each_line do |line|
        line = line.chomp
        line = line[1..-1] if line[0, 1] == "\uFEFF"   # 掉 BOM
        next if line.empty? || line[0, 1] == "#"
        tab = line.index("\t")
        next if tab.nil?
        key = line[0, tab]
        val = line[(tab + 1)..-1]
        next if key.nil? || val.nil? || key.empty? || val.empty?
        @dict[key] = val
        # 顺手记最长 key：片段匹配的窗口上限，见 `lookup_frag`。
        klen = key.length
        @max_frag = klen if klen > @max_frag
      end
    end
    log "词典载入 #{@dict.size} 条，用时 #{(Time.now - t0).round(2)}s"
    true
  end

  #--------------------------------------------------------------------------
  # ● 查一条（精确为主，控制码剥离为辅）
  #--------------------------------------------------------------------------
  # 返回译文；没命中返回 nil。
  #
  # 统一到 UTF-8 再查表。
  #
  # 词典是从文件按 "r:utf-8" 读进来的（UTF-8 标记），而游戏里的字符串来自
  # .rvdata2 —— 两者若编码标记不一致，即使内容一模一样，Hash 也查不中
  # （Ruby 1.9 的字符串相等要求编码可兼容）。所以先归一。
  # 正常情况（两边都是 UTF-8）零开销，直接返回原对象。
  # 注意：**不要引用 Encoding::UTF_8 这类常量**。实测目标游戏的 RGSS 环境里
  # `Encoding` 常量不存在（启动即 NameError，游戏直接崩），只能按编码**名字**
  # 比较。整套操作都包在 rescue 里：拿不到编码信息就当已经是 UTF-8。
  def self.as_utf8(s)
    return s unless s.respond_to?(:encoding)
    enc = s.encoding.to_s
    return s if enc == "UTF-8"
    if enc == "ASCII-8BIT" || enc == "BINARY" || enc == "US-ASCII"
      s.dup.force_encoding("UTF-8")
    else
      begin
        s.encode("UTF-8")
      rescue Exception
        s.dup.force_encoding("UTF-8")
      end
    end
  rescue Exception
    s
  end

  # 三级退级，和 MZ 桥的 Dict::translate 一个思路：
  #   1. 整串精确查；
  #   2. 剥掉控制码再查（入参可能还带着 \c[2] / \eC[2]）；
  #   3. 片段匹配 —— 这是 RGSS3 侧覆盖率的关键。
  #
  # 第 1、2 级都算「整段匹配」，命中就是可靠的；只有落到第 3 级才写告警
  # （见 `lookup_frag`），因为那一层是拼接替换，比整段匹配容易翻坏。
  #
  # 为什么非要有第 3 级：MTool 的 key 是它自己在绘制层抓的那些串，而游戏
  # 实际画出来的常常是「key + 装饰」拼起来的：
  #     「どのファイルをロードしますか？」（词典只有不带 ？ 的那条）
  #     「ルカのレベル:88」「収集率：51%」「ファイル 5」「ページ 1/2」
  #     「アリスフィーズ（小）」（词典里只有「アリスフィーズ」）
  # 只做精确查，这些整屏都是日文 —— 存档/读档界面就是这么全灭的。
  def self.lookup(s)
    return nil unless s.is_a?(String)
    s = as_utf8(s)
    return nil if s.length < 2
    return nil unless s =~ NEEDS_TR

    v = @dict[s]
    return v if v

    bare = s
    bare = bare.gsub(ESC_CODE, "") if bare =~ ESC_CODE
    bare = bare.gsub(RAW_CODE, "") if bare =~ RAW_CODE
    bare = bare.gsub("\e", "").strip
    if bare != s && bare.length >= 2
      v = @dict[bare]
      return v if v
      s = bare
    end

    lookup_frag(s)
  end

  # 从左往右贪心取「最长且命中词典」的片段替换；没命中就原样推进一格。
  #
  # 取最长是为了避免「ルカのレベル」被更短的片段切开翻错；一格一格推进，
  # 保证一条串里的多个片段都能翻到（例：`A：B` 两段各查各的）。
  #
  # 这里**不做任何假名/内容词限制** —— 只要求片段 ≥ 2 字、且在词典里。
  # 曾经加过两道关卡（两侧非假名、片段须含汉字或片假名），本意是防「する => 是」
  # 这类虚词误替换，但副作用太大：`おねだり1（尻尾吸精）` 里的 `おねだり` 是纯
  # 平假名，被整个挡在门外，本来翻得出来的地方翻不出来了。
  # 现在的取舍是**覆盖率优先**，误替换靠 `片段回退` 告警事后核对。
  # 结果按原文缓存 —— UI 每帧重画同一批标签，缓存把这层开销摊成一次性。
  #
  # 唯一的例外是「短键长译」（见下）—— 那不是词义问题，是词典条目本身错位。
  #
  # 这套守卫和 MZ 侧 `Dict::fragments`（src-tauri/src/dict/mtool.rs）里那份
  # **必须保持一致**：两边吃的是同一份词典，行为不该分叉。
  def self.lookup_frag(s)
    cached = @frag[s]
    return cached if cached != nil
    return nil if @frag.key?(s)

    @frag.clear if @frag.size >= CACHE_MAX

    out = nil
    buf = ""
    i = 0
    n = s.length
    while i < n
      len = n - i
      len = @max_frag if len > @max_frag
      m = nil
      while len >= 2
        v = @dict[s[i, len]]
        if v
          # 「短键长译」：key 只有两三个字，译文却是一整句 —— 这不是词义，是
          # 词典条目本身错位（绘制层串了位）。实测这份词典 562595 条里只有 1 条：
          #     `れる` => `这种触手让你高潮时，生命力也会一起被吸走哦`
          # 让它参与片段拼接，就会把这整句塞进单词中间（`激しく` + 它 + 后面）。
          # 跳过它，这一格原样推进就是干净的结果。
          # 整段/单行两级不受影响 —— 那两级是精确匹配，词典说什么就是什么。
          if v.length >= 8 && v.length >= len * 4
            len -= 1
            next
          end
          m = [len, v]
          break
        end
        len -= 1
      end
      if m
        buf << m[1]
        i += m[0]
      else
        buf << s[i, 1]
        i += 1
      end
    end

    out = buf if buf != s

    # 能走到这里，说明整段没命中、退到了片段替换 —— 这条比整段匹配容易翻坏，
    # 所以每次真的替换出东西就写一条告警，事后好核对。
    # 下一行会把结果按原文缓存，所以同一条串只告警一次；窗口每帧重画同一批
    # 标签，不会把日志刷爆。
    if out
      @frag_warn += 1
      log("片段回退[#{@frag_warn}] #{s}  =>  #{out}") if @frag_warn <= FRAG_LOG_MAX
    end

    @frag[s] = out
    out
  end

  # 整个查表路径包一层兜底：钩子挂在绘制函数里，任何异常都会直接崩游戏。
  # 查不动就原样返回，最坏是这条没翻，也不能让玩家玩不下去。
  def self.tr(s)
    return s unless s.is_a?(String)
    # 已经是译文了就别再翻一遍。全类扫描会把同一个串经好几层绘制函数（子类
    # 的钩子 → 基类的钩子），第二遍拿到的已经是中文，再查表只会误伤。
    return s if @trans[s]

    v = lookup(s)
    if v.nil?
      log_miss(s)
      return s
    end
    @trans[v] = true if @trans.size < 200000
    @hits += 1
    log("命中[#{@hits}] #{s}  =>  #{v}") if @hits <= @sample_limit
    v
  rescue Exception
    s
  end

  # 该翻却没翻出来的原文记一份，供事后补词典。
  #
  # `lookup` 已经挡住了「纯 ASCII / 太短」的串，走到这里还返回 nil 就说明
  # 确实是「看着该翻但词典里没有」。按原文去重、有总量上限 —— 窗口每帧重画
  # 同一批标签，不去重的话几秒钟就能把日志刷成几百兆。
  def self.log_miss(s)
    return nil unless s.is_a?(String)
    t = as_utf8(s)
    return nil if t.length < 2 || !(t =~ NEEDS_TR)
    return nil if @miss.key?(t) || @miss.size >= MISS_MAX
    @miss[t] = true
    File.open(MISS_PATH, "a:utf-8") { |f| f.puts(t) }
  rescue Exception
    nil
  end

  # 多行文本逐行翻（对话 all_text 就是 \n 连起来的显示行）
  def self.tr_lines(s)
    return s unless s.is_a?(String)
    return tr(s) unless s.include?("\n")
    s.split("\n", -1).map { |ln| tr(ln) }.join("\n")
  end

  #--------------------------------------------------------------------------
  # ● 安装：全类扫描
  #--------------------------------------------------------------------------
  def self.owns_method?(klass, m)
    klass.instance_methods(false).include?(m) ||
      klass.private_instance_methods(false).include?(m) ||
      klass.protected_instance_methods(false).include?(m)
  rescue Exception
    false
  end

  def self.has_method?(klass, m)
    klass.method_defined?(m) || klass.private_method_defined?(m) || klass.protected_method_defined?(m)
  rescue Exception
    false
  end

  # 按「文本在 args 的第几格」造一个包装方法。
  # 找不到文本就原样转发，绝不替原方法做主。
  def self.build_arg_hook(klass, m, al, slot)
    klass.class_eval do
      define_method(m) do |*args|
        begin
          if slot == :all
            # 签名不定的那些（draw_text 有 rect 版和 5 参数版），全参数扫一遍，
            # 凡是 String 都试着换 —— MTool 就是这么干的。
            na = []
            args.each { |x| na.push(x.is_a?(String) ? WTool.tr(x) : x) }
            send(al, *na)
          else
            if args.length > slot && args[slot].is_a?(String)
              na = args.dup
              na[slot] = WTool.tr(na[slot])
              send(al, *na)
            else
              send(al, *args)
            end
          end
        rescue Exception
          # 换文本这一步出的任何问题，都退回原参数重画，不能让游戏崩。
          send(al, *args)
        end
      end
    end
  end

  # 对话入口：在消息刚填进 $game_message.texts 时就换成译文。
  # 这样下游的逐字绘制、行宽行高计算全部自动拿到译文，不用碰绘制逻辑。
  def self.build_start_message(klass, al)
    klass.class_eval do
      define_method(:start_message) do |*args|
        begin
          texts = $game_message.texts
          if texts && !texts.empty?
            new_msg = []
            texts.each { |i| new_msg.push(i.is_a?(String) ? WTool.tr(i) : i) }
            texts.clear
            new_msg.each { |msg| texts.push(msg) }
          end
        rescue Exception
          # 取不到 $game_message 就交给原方法处理
        end
        send(al, *args)
      end
    end
  end

  # 给一个类挂一个方法。返回是否真的挂上了。
  #
  # 只用 `instance_methods(false)`（本类自己定义的）判断：子类没 override 的
  # 会继承基类那份钩子，不需要重复挂；重复挂反而会叠出多层查表。
  # alias 名带 object_id，保证同一方法不会被挂两遍（重装时靠它幂等）。
  def self.install_hook(klass, m)
    name = klass.to_s
    return false if name.empty?
    # Bitmap#draw_text 单独处理（见 install 末尾）：它是逐字绘制的底层出口，
    # 挂上通用版会把对话按单字查表。MTool 同样跳过它。
    return false if name == "Bitmap" && m == :draw_text
    return false unless owns_method?(klass, m)

    al = ("__wtool_" + klass.object_id.to_s + "_" + m.to_s).to_sym
    return false if has_method?(klass, al)

    begin
      klass.class_eval { alias_method al, m }
    rescue Exception
      return false
    end

    begin
      if m == :start_message
        build_start_message(klass, al)
      else
        build_arg_hook(klass, m, al, TEXT_ARG[m])
      end
    rescue Exception
      return false
    end
    true
  end

  # 把字体名换成 FONT_NAME。入参可能三种形态：
  #   "Arial"            → 换
  #   ["Arial", 20]      → 只换名字，保留字号（RGSS3 的 Font.new 简写）
  #   20                 → 字号，不动
  #
  # 数组必须 dup：`Font.default_name` 返回的就是游戏自己持有的那个数组，原地改
  # 会污染游戏配置（它后面可能还要读回来）。MTool 那版直接原地改并 slice 掉
  # 多余元素，会连字号一起丢掉，这里不跟。
  def self.sub_font(v)
    n = font_name
    return v if n.nil? || n.empty?
    return n if v.is_a?(String)
    if v.is_a?(Array)
      a = v.dup
      a[0] = n if a[0].is_a?(String)
      return a
    end
    v
  end

  #--------------------------------------------------------------------------
  # ● 换字体
  #--------------------------------------------------------------------------
  # 三个口子都要堵住，缺一个就会漏：
  #   Font.new            程序里现建的字体（大多数窗口走这条）
  #   Font#name=          拿到 Font 对象后单独改名字
  #   Font.default_name=  游戏在 Main 里设的全局默认 —— MTool 没挂这处，
  #                       因为它 DLL 层还截了 CreateFontIndirectW 兜底；
  #                       我们没有那一层，必须在这儿补上。
  def self.install_font
    unless defined?(Font)
      log "警告：Font 未定义，字体未替换"
      return
    end
    return if font_name.nil? || font_name.empty?

    class << Font
      unless method_defined?(:wtool_orig_font_new)
        alias_method :wtool_orig_font_new, :new
        def new(*args, &blk)
          args[0] = WTool.sub_font(args[0]) unless args.empty?
          wtool_orig_font_new(*args, &blk)
        end
      end

      unless method_defined?(:wtool_orig_default_name_eq)
        alias_method :wtool_orig_default_name_eq, :default_name=
        def default_name=(v)
          wtool_orig_default_name_eq(WTool.sub_font(v))
        end
      end
    end

    Font.class_eval do
      unless method_defined?(:wtool_orig_name_eq)
        alias_method :wtool_orig_name_eq, :name=
        def name=(v)
          wtool_orig_name_eq(WTool.sub_font(v))
        end
      end
    end

    log "字体已替换为「#{font_name}」"
  end

  def self.install
    # 实时模式：字体以 FONT_PATH 指向的文件为准，先读一次（读不到就回退 FONT_NAME）。
    read_font_file if LIVE

    ok = load_dict

    install_font

    # 全类扫描：游戏里所有已加载的类，谁自己定义了这些方法就给谁挂一份。
    # 每个类只取一次方法表（几千个类 × 7 个方法，不合并的话方法表要算好几万次）。
    scanned = 0
    begin
      ObjectSpace.each_object(Class) do |klass|
        next if klass.to_s.empty?
        begin
          own = klass.instance_methods(false) +
                klass.private_instance_methods(false) +
                klass.protected_instance_methods(false)
        rescue Exception
          next
        end
        HOOK_METHODS.each do |m|
          next unless own.include?(m)
          scanned += 1 if install_hook(klass, m)
        end
      end
    rescue Exception => e
      log "全类扫描异常：#{e.class}: #{e.message}"
    end
    log "全类扫描挂载 #{scanned} 处"

    # 对话：整段文本逐字绘制，只能在「整段刚拼好」这一刻换掉。
    # 这一处是改写绘制逻辑（不是简单包装），所以不进通用扫描，只挂 Window_Message。
    if defined?(Window_Message) && Window_Message.method_defined?(:process_all_text)
      Window_Message.class_eval do
        unless method_defined?(:wtool_orig_process_all_text)
          alias_method :wtool_orig_process_all_text, :process_all_text
          # 对话是逐字绘制（process_character 每字一次 draw_text），
          # 只能在「整段文本刚拼好」这一刻换掉。原文里可能带 \v[n]，
          # 所以先把本地那份换成译文版再进逐字循环。
          def process_all_text
            open_and_wait
            text = WTool.tr_lines(convert_escape_characters($game_message.all_text))
            pos = {}
            new_page(text, pos)
            process_character(text.slice!(0, 1), text, pos) until text.empty?
          end
        end
      end
    else
      log "警告：Window_Message#process_all_text 不可用，对话 hook 未安装"
    end

    # Bitmap 层兜底：有些窗口直接调 contents.draw_text，绕过 Window_Base#draw_text
    # （存档/读档、自定义图鉴这类最容易这么写）。
    #
    # 这一层**只处理含假名的串**，两道保险：
    #   1. 我们换出来的译文是中文、不含假名 → 不会被二次翻译；
    #   2. 逐字绘制的对话每个字符长度 1 → 被 lookup 的长度守卫挡掉，
    #      不会像 MZ 桥那样因为「单字查表」把文本画错位。
    if defined?(Bitmap)
      Bitmap.class_eval do
        unless method_defined?(:wtool_orig_bm_draw_text)
          alias_method :wtool_orig_bm_draw_text, :draw_text
          # Bitmap#draw_text(x, y, width, height, str, align = 0)，文本固定在 4。
          def draw_text(*args)
            t = args[4]
            if t.is_a?(String) && t =~ WTool::KANA
              args[4] = WTool.tr(t)
            end
            wtool_orig_bm_draw_text(*args)
          end
        end
      end
    else
      log "警告：Bitmap 未定义，底层 hook 未安装"
    end

    log "hook 安装完成 v#{VERSION}（词典 #{@dict.size} 条，#{ok ? '已载入' : '缺失'}）"

    # 实时模式才装热重载：一次性模式读完这一次就是终态，没有轮询的必要。
    install_tick if LIVE

    true
  end
end

WTool.log "=== wtool hook 启动 v#{WTool::VERSION} ==="
begin
  WTool.install
rescue Exception => e
  begin
    File.open(ERR_PATH, "w:utf-8") do |f|
      f.puts "#{e.class}: #{e.message}"
      f.puts e.backtrace
    end
  rescue Exception
  end
end