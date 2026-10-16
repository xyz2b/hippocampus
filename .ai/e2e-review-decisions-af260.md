# Claude 问题级人工复核（AF260）：端到端小子集 A/B

## A（生产配置，R4=DS-V4-Pro）
accepted：lme_gpt4_b5700ca9、time_q1、time_q3、device_q1、device_q3、event_q5、correction_q1、correction_q2、organization_q6、person_q3、task_q1、task_q2、conflict_q2（只给冲突说明，未选值）。
自动失败（机械核验，不可豁免）：lme_gpt4_6dc9b45b（R4 model_invalid_output，107 s、6.8k 推理）、device_q5（R4 model_invalid_output）。
结果：13/15。

## B（R4=GPT-6.1 Sol）
accepted：lme_gpt4_6dc9b45b、lme_gpt4_b5700ca9、time_q1、time_q3、device_q1、device_q3、device_q5、event_q5、correction_q1、correction_q2、person_q3（organization_q6 自动通过）。
rejected：unresolved_conflict/conflict_q2——在冲突状态下另给一条“屏幕尺寸是7英寸 current=yes”，等于选出一个当前值（违反 forbidden）。
自动失败：task_q1、task_q2——任务范围内的配置要求判 insufficient、applies=unknown，过度保守。
结果：12/15。
