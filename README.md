一个模仿mtool的多游戏引擎程序嵌入式实时翻译系统。
当前只支持rpgmaker mv


D:\\myProgram\\game2\\lilisproject\\SUCCUBUS DESIRE ~リリスプロジェクト~
上面是用来测试的程序

大体分为
1 平台适配层。//暂时不考虑
2 翻译读取层。
3 指令层/前端层。


engine提供各个引擎的标准接口
model 提供数据定义
task 提供主线程
commands 提供前端直接调用的接口函数

