# 其余 59 题标注（已由用户确认，reviewed）

全部为 `label_status: reviewed`（2026-09-30 用户确认）。依据原文与提问时点起草。禁止项除跨任务来源外只约束本题答案；正确的历史与补充上下文作为额外结论进复核。

| # | 题 | 问题 | 状态 | 期望要点 | 禁止项 | 备注 |
|---|---|---|---|---|---|---|
| 1 | lme_0862e8bf_abs/q | What is the name of my hamster? | insufficient（unspecified/lookup） | — | wrong_value ['Luna'] | 原文只有猫叫 Luna，没有仓鼠。 |
| 2 | lme_10e09553/q | How many largemouth bass did I catch with Alex on the earlier fishing trip to Lake Michigan before the 7/22 trip? | answer（past/lookup） | self：7 / 7 largemouth bass ← ['s1_t2'] | wrong_value ['9', '9 largemouth bass'] | 较早一次为 7/10（s1_t2，7 条）；7/22 那次为 9 条（s2_t2、s2_t5），不能答 9。 |
| 3 | lme_15745da0_abs/q | How long have I been collecting vintage films? | insufficient（unspecified/lookup） | — | wrong_value ['three months', '3 months'] | 原文是收藏老相机三个月，不是老电影。 |
| 4 | lme_19b5f2b3_abs/q | How long was I in Korea for? | insufficient（unspecified/lookup） | — | wrong_value ['two weeks', '2 weeks'] | 原文两周是在日本；韩国只是打算去。 |
| 5 | lme_29f2956b_abs/q | How much time do I dedicate to practicing violin every day? | insufficient（unspecified/lookup） | — | wrong_value ['30 minutes'] | 原文每天 30 分钟练的是吉他，不是小提琴。 |
| 6 | lme_577d4d32/q | What time do I stop checking work emails and messages? | answer（current/lookup） | self：7 pm / 7pm / 7:00 pm ← ['s1_t3']，current=yes | — |  |
| 7 | lme_5831f84d/q | How many Crash Course videos have I watched in the past few weeks? | answer（current/lookup） | self：15 / 15 videos / 15 Crash Course videos ← ['s2_t1']，current=yes | value_as_current ['10', '12'] | 知识更新：10（s1_t1）→ 第 12 个（s1_t5）→ 15（s2_t1）。原文直接给出数量，按取值题处理。 |
| 8 | lme_603deb26/q | How many times have I tried making a Negroni at home since my friend Emma showed me how to make it? | answer（current/lookup） | self：10 / 10 times ← 任一 ['s2_t1', 's2_t4', 's2_t6']，current=yes | value_as_current ['5', '5 times'] | 知识更新：5 次（s1_t3）→ 10 次（s2_t1/t4/t6）。 |
| 9 | lme_6f9b354f/q | What color did I repaint my bedroom walls? | answer（past/lookup） | self：a lighter shade of gray / lighter shade of gray / a lighter gray ← ['s1_t2'] | — |  |
| 10 | lme_71017276/q | How many weeks ago did I meet up with my aunt and receive the crystal chandelier? | answer（past/elapsed） | self：（事件） ← ['s1_t1']<br>elapsed 4 week（nearest_week，28 天）：2023-03-04[s1_t1] → 2023-04-01[提问时刻] | — |  |
| 11 | lme_76d63226/q | What size is my new Samsung TV? | answer（current/lookup） | self：55-inch / 55 inch / 55 inches ← ['s1_t3']，current=yes | — |  |
| 12 | lme_7a87bd0c/q | How long have I been sticking to my daily tidying routine? | answer（current/lookup） | self：4 weeks / four weeks ← ['s2_t1']，current=yes | value_as_current ['3 weeks', 'three weeks'] | 知识更新：3 周（s1_t1、s1_t5，2023-06-11）→ 4 周（s2_t1，2023-09-30）。原文直接给出时长，不做日期换算。 |
| 13 | lme_a2f3aa27/q | How many followers do I have on Instagram now? | answer（current/lookup） | self：1300 ← ['s2_t1']，current=yes，不确定表达 'I think' | value_as_current ['1250'] | 知识更新：1250（s1_t2）→ “I think I'm close to 1300”（s2_t1）。value_any 只放裸值 1300；带“close to/about”等限定的答案不作整串等值，送复核判断是否保留了原文的估计语气。 |
| 14 | lme_af082822/q | How many weeks ago did I attend the friends and family sale at Nordstrom? | answer（past/elapsed） | self：（事件） ← ['s1_t2']<br>elapsed 2 week（nearest_week，14 天）：2022-11-17[s1_t2] → 2022-12-01[提问时刻] | — |  |
| 15 | lme_af8d2e46/q | How many shirts did I pack for my 5-day trip to Costa Rica? | answer（past/lookup） | self：7 / 7 shirts ← ['s1_t4'] | wrong_value ['3', '3 shirts'] | 原文只说“last trip to Costa Rica”带了 7 件、穿了 3 件；可答 7 件，但不能据此确认旅行为 5 天。wrong_value 只禁止把 3 作为本题答案，“带了 7 件、实际穿了 3 件”这类正确补充不算错。 |
| 16 | lme_b6019101/q | How many MCU films did I watch in the last 3 months? | answer（current/lookup） | self：5 / 5 MCU films ← ['s2_t1']，current=yes，条件 in the last 3 months（问题已限定） | value_as_current ['4', '4 MCU films']；wrong_value ['12', '12 films'] | 知识更新：4 部（s1_t3）→ 5 部（s2_t1，同句 12 部是全部电影）。 |
| 17 | lme_bc8a6e93_abs/q | What did I bake for my uncle's birthday party? | insufficient（unspecified/lookup） | — | wrong_value ['lemon blueberry cake', 'a lemon blueberry cake'] | 原文的柠檬蓝莓蛋糕是给侄女/外甥女的生日会做的，不是叔叔。 |
| 18 | lme_bcbe585f/q | How many weeks ago did I attend a bird watching workshop at the local Audubon society? | answer（past/elapsed） | self：（事件） ← ['s1_t1']<br>elapsed 4 week（nearest_week，30 天，近似）：2023-04-01[s1_t1] → 2023-05-01[提问时刻] | — |  |
| 19 | lme_c8c3f81d/q | What brand are my favorite running shoes? | answer（current/lookup） | self：Nike ← ['s1_t4']，current=yes | — |  |
| 20 | lme_ccb36322/q | What is the name of the music streaming service have I been using lately? | answer（current/lookup） | self：Spotify ← ['s1_t2']，current=yes | — |  |
| 21 | lme_e47becba/q | What degree did I graduate with? | answer（past/lookup） | self：Business Administration / a degree in Business Administration ← ['s1_t3'] | — |  |
| 22 | lme_f4f1d8a4_abs/q | What did my dad gave me as a birthday gift? | insufficient（unspecified/lookup） | — | wrong_value ['stand mixer', 'a stand mixer', 'a new stand mixer'] | 原文的厨师机是姐姐/妹妹送的生日礼物，不是爸爸。 |
| 23 | lme_gpt4_6dc9b45b/q | How many months ago did I attend the Seattle International Film Festival? | answer（past/elapsed） | self：（事件） ← ['s1_t1']<br>elapsed 4 month（complete_calendar_months，123 天）：2021-06-01[s1_t1] → 2021-10-02[提问时刻] | — | 起点取 s1_t1 “today” 看 Coda 的日期，这是明确参与电影节的日期，不是电影节开始日期（原文另说参加了一周）。 |
| 24 | lme_gpt4_b5700ca9/q | How many days ago did I attend the Maundy Thursday service at the Episcopal Church? | answer（past/elapsed） | self：（事件） ← ['s1_t4']<br>elapsed 4 day（calendar_days，4 天）：2023-04-06[s1_t4] → 2023-04-10[提问时刻] | — |  |
| 25 | lme_gpt4_e072b769/q | How many weeks ago did I start using the cashback app 'Ibotta'? | insufficient（past/elapsed） | — | — | 原文只说 2023-04-16 “I've just downloaded Ibotta … I want to make sure I get the most out of it”，下载不等于开始使用，开始使用的日期证据不足。原始 LongMemEval 期望“3 weeks ago”保留在 expect 中，报告需说明两种口径的差异；20 天约 3 周的算术本身正确。 状态为 insufficient 时仍可返回可信的补充事实（如“4月16日的消息里提到刚下载 Ibotta，首次使用日期没有记录”），作为额外结论进复核；若提到 20 天，须说明是距该条消息的时间，不能说成距下载或首次使用的精确时间。computed 保持 null。 |
| 26 | absolute_date_ranges/time_q1 | 2027年4月14日，岸芷艺术馆几点闭馆？ | answer（at_time/lookup） | organization_anzhi_gallery：17:30 ← ['time_s1'] | wrong_value ['19:00'] |  |
| 27 | absolute_date_ranges/time_q2 | 2027年4月15日，岸芷艺术馆几点闭馆？ | answer（at_time/lookup） | organization_anzhi_gallery：19:00 ← ['time_s2'] | wrong_value ['17:30'] |  |
| 28 | absolute_date_ranges/time_q3 | 2027年5月1日，岸芷艺术馆几点闭馆？ | insufficient（at_time/lookup） | — | wrong_value ['17:30', '19:00'] | 5/1 不在两段已定安排之内。 |
| 29 | device_default_paper/device_q1 | 默认纸张规格是什么？ | clarify（current/lookup） | — | — |  |
| 30 | device_default_paper/device_q2 | 默认纸张规格是什么？ ［上下文：我指的是那台打印机。］ | clarify（current/lookup） | — | — |  |
| 31 | device_default_paper/device_q3 | 默认纸张规格是什么？ ［上下文：我指的是雾岚打印机。］ | answer（current/lookup） | device_wulan_printer：A4 ← ['device_s1']，current=yes | wrong_subject_source ['device_s2']；value_as_current ['B5'] |  |
| 32 | device_default_paper/device_q4 | 青苔打印机的默认纸张规格是什么？ | answer（current/lookup） | device_qingtai_printer：B5 ← ['device_s2']，current=yes | wrong_subject_source ['device_s1']；value_as_current ['A4'] |  |
| 33 | device_default_paper/device_q5 | 不改纸张设置时，雾岚打印机会按哪种尺寸打印？ | answer（current/lookup） | device_wulan_printer：A4 ← ['device_s1']，current=yes | wrong_subject_source ['device_s2']；value_as_current ['B5'] |  |
| 34 | device_default_paper/device_q6 | 青苔打印机使用什么型号的墨盒？ | insufficient（current/lookup） | — | wrong_value ['A4', 'B5'] |  |
| 35 | event_registration_fee/event_q1 | 报名费是多少？ | clarify（current/lookup） | — | — |  |
| 36 | event_registration_fee/event_q2 | 报名费是多少？ ［上下文：我说的是折纸工作坊。］ | clarify（current/lookup） | — | — |  |
| 37 | event_registration_fee/event_q3 | 报名费是多少？ ［上下文：我说的是海盐折纸工作坊。］ | answer（current/lookup） | event_haiyan_origami：36 / 36元 / 36 元 ← ['event_s1']，current=yes | wrong_subject_source ['event_s2']；value_as_current ['58', '58元'] |  |
| 38 | event_registration_fee/event_q4 | 银杏折纸工作坊的报名费是多少？ | answer（current/lookup） | event_yinxing_origami：58 / 58元 / 58 元 ← ['event_s2']，current=yes | wrong_subject_source ['event_s1']；value_as_current ['36', '36元'] |  |
| 39 | event_registration_fee/event_q5 | 参加海盐折纸工作坊，报名要交多少钱？ | answer（current/lookup） | event_haiyan_origami：36 / 36元 / 36 元 ← ['event_s1']，current=yes | wrong_subject_source ['event_s2']；value_as_current ['58', '58元'] |  |
| 40 | event_registration_fee/event_q6 | 银杏折纸工作坊最多招收多少人？ | insufficient（current/lookup） | — | wrong_value ['36', '58', '36元', '58元'] |  |
| 41 | explicit_correction/correction_q1 | 墨湾相机使用多大容量的存储卡？ | answer（current/lookup） | device_mowan_camera：64GB / 64 GB ← ['correction_s1']，current=yes | wrong_source ['correction_s2'] | 提问时 correction_s2 尚未出现，64GB 当时正确。 wrong_source 保留：该来源在提问时尚不存在，答案不能以它为依据。 |
| 42 | explicit_correction/correction_q2 | 墨湾相机使用多大容量的存储卡？ | answer（current/lookup） | device_mowan_camera：128GB / 128 GB ← ['correction_s2']，current=yes | value_as_current ['64GB', '64 GB'] | correction_s2 为纠错：64GB 不是曾经成立的历史值，不应作为当前结论或历史事实出现（contracts 不变量 7）。 |
| 43 | explicit_correction/correction_q3 | 现在确认墨湾相机的存储卡是64GB还是128GB？ | answer（current/lookup） | device_mowan_camera：128GB / 128 GB ← ['correction_s2']，current=yes | value_as_current ['64GB', '64 GB'] | correction_s2 为纠错：64GB 不是曾经成立的历史值，不应作为当前结论或历史事实出现（contracts 不变量 7）。 |
| 44 | organization_borrow_limit/organization_q1 | 每人同时最多能借几本书？ | clarify（current/lookup） | — | — |  |
| 45 | organization_borrow_limit/organization_q2 | 每人同时最多能借几本书？ ［上下文：我问的是那个读书会。］ | clarify（current/lookup） | — | — |  |
| 46 | organization_borrow_limit/organization_q3 | 每人同时最多能借几本书？ ［上下文：我问的是白鹭读书会。］ | answer（current/lookup） | organization_bailu_bookclub：3 / 3册 / 3本 / 3 册 ← ['organization_s1']，current=yes | wrong_subject_source ['organization_s2']；value_as_current ['5', '5册', '5本'] |  |
| 47 | organization_borrow_limit/organization_q4 | 赤枫读书会每名会员同时最多能借几册书？ | answer（current/lookup） | organization_chifeng_bookclub：5 / 5册 / 5本 / 5 册 ← ['organization_s2']，current=yes | wrong_subject_source ['organization_s1']；value_as_current ['3', '3册', '3本'] |  |
| 48 | organization_borrow_limit/organization_q5 | 在白鹭读书会，一名会员手上最多可以有几本尚未归还的书？ | answer（current/lookup） | organization_bailu_bookclub：3 / 3册 / 3本 / 3 册 ← ['organization_s1']，current=yes | wrong_subject_source ['organization_s2']；value_as_current ['5', '5册', '5本'] |  |
| 49 | organization_borrow_limit/organization_q6 | 赤枫读书会每本书可以借几天？ | insufficient（current/lookup） | — | wrong_value ['3', '5', '3册', '5册'] |  |
| 50 | person_sketch_pen/person_q1 | 画速写时固定用哪种笔？ | clarify（current/lookup） | — | — |  |
| 51 | person_sketch_pen/person_q2 | 画速写时固定用哪种笔？ ［上下文：我问的是姓冯的那位。］ | clarify（current/lookup） | — | — |  |
| 52 | person_sketch_pen/person_q3 | 画速写时固定用哪种笔？ ［上下文：我问的是冯砚川。］ | answer（current/lookup） | person_feng_yanchuan：炭笔 ← ['person_s1']，current=yes，条件 画速写时（问题已限定），不确定表达 None | wrong_subject_source ['person_s2']；value_as_current ['钢笔'] |  |
| 53 | person_sketch_pen/person_q5 | 冯砚川画速写拿的是哪种笔？ | answer（current/lookup） | person_feng_yanchuan：炭笔 ← ['person_s1']，current=yes，条件 画速写时（问题已限定），不确定表达 None | wrong_subject_source ['person_s2']；value_as_current ['钢笔'] |  |
| 54 | person_sketch_pen/person_q6 | 冯序宁画速写通常花多长时间？ | insufficient（current/lookup） | — | wrong_value ['炭笔', '钢笔'] |  |
| 55 | task_isolation/task_q1 | 折页导航终端开机应显示哪种语言？ | answer（current/lookup） | device_zheye_terminal：西班牙语 ← ['task_s1']，current=yes | source_out_of_scope ['task_s2']；value_as_current ['意大利语'] |  |
| 56 | task_isolation/task_q2 | 折页导航终端开机应显示哪种语言？ | answer（current/lookup） | device_zheye_terminal：意大利语 ← ['task_s2']，current=yes | source_out_of_scope ['task_s1']；value_as_current ['西班牙语'] |  |
| 57 | task_isolation/task_q3 | 继续潮汐展陈任务的展台演示配置：折页导航终端应沿用意大利语作为开机显示语言吗？ | answer（current/lookup） | device_zheye_terminal：西班牙语 ← ['task_s1']，current=yes | source_out_of_scope ['task_s2']；value_as_current ['意大利语'] | task_q3 问法暗示意大利语，正确回答是不沿用、应为西班牙语。 |
| 58 | unresolved_conflict/conflict_q1 | 栖风读书机的屏幕是几英寸？ | answer（current/lookup） | device_qifeng_reader：7英寸 / 7 / 7 英寸 ← ['conflict_s1']，current=yes | wrong_source ['conflict_s2'] | 提问时 conflict_s2 尚未出现，7 英寸当时正确（按其时点标注）。 wrong_source 保留：该来源在提问时尚不存在，答案不能以它为依据。 |
| 59 | unresolved_conflict/conflict_q3 | 能确定栖风读书机是7英寸屏幕吗？ | conflict（current/lookup） | 冲突成员：7英寸/7；8英寸/8 | value_as_current ['7英寸', '7']；value_as_current ['8英寸', '8'] | 问法是“能否确定是 7 英寸”；正确处理是报告冲突，不能确认 7 英寸，也不能改答 8 英寸。 |
