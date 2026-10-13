//! Deterministic skill-source previews. This peer never contacts a network source.
use orbit_protocol::resources::{Action, Skill, SkillCandidate, Snapshot};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(super) const EXISTING_CONTENT: &str =
    "---\nname: review\ndescription: Existing project review\n---\nPreserve this local skill.\n";

pub(super) fn seed(skills: &mut HashMap<String, String>) {
    skills
        .entry("review".into())
        .or_insert_with(|| EXISTING_CONTENT.into());
}

struct Preview {
    session: String,
    workspace: String,
    skills: Vec<SkillCandidate>,
}

#[derive(Default)]
pub(super) struct FixtureImports {
    sequence: u64,
    previews: HashMap<String, Preview>,
}

impl FixtureImports {
    pub(super) fn execute(
        &mut self,
        session: &str,
        workspace: &str,
        action: &Value,
        installed: &mut HashMap<String, String>,
    ) -> Option<Value> {
        if action.get("ScanSkills").is_none() && action.get("ImportSkills").is_none() {
            return None;
        }
        let result = serde_json::from_value(action.clone())
            .map_err(anyhow::Error::from)
            .and_then(|action| self.apply(session, workspace, action, installed));
        Some(match result {
            Ok(snapshot) => json!({"type":"resources","value":snapshot}),
            Err(error) => json!({"type":"error","value":error.to_string()}),
        })
    }

    fn apply(
        &mut self,
        session: &str,
        workspace: &str,
        action: Action,
        installed: &mut HashMap<String, String>,
    ) -> anyhow::Result<Snapshot> {
        match action {
            Action::ScanSkills { source } => {
                let source = source.trim();
                anyhow::ensure!(!source.is_empty(), "请输入技能来源");
                anyhow::ensure!(source != "bad-source", "无法读取技能来源，请检查地址后重试");
                self.sequence += 1;
                let token = format!("fixture-skill-source-{}", self.sequence);
                let skills = [
                    (
                        "skills/review",
                        "review",
                        "代码审查",
                        "保留项目中的现有审查流程。",
                    ),
                    (
                        "skills/frontend-design",
                        "frontend-design",
                        "界面设计",
                        "设计清晰、一致的桌面界面，包含示例与参考资料。",
                    ),
                    (
                        "skills/工具/workflow",
                        "workflow",
                        "项目工作流",
                        "包含 scripts、references 与 assets 的完整技能目录。",
                    ),
                ]
                .into_iter()
                .map(|(path, name, title, description)| SkillCandidate {
                    path: path.into(),
                    name: name.into(),
                    title: title.into(),
                    description: description.into(),
                    conflict: installed.contains_key(name),
                })
                .collect::<Vec<_>>();
                self.previews.insert(
                    token.clone(),
                    Preview {
                        session: session.into(),
                        workspace: workspace.into(),
                        skills: skills.clone(),
                    },
                );
                Ok(Snapshot::SkillSource {
                    token,
                    source: source.into(),
                    skills,
                })
            }
            Action::ImportSkills { token, paths } => {
                let preview = self
                    .previews
                    .remove(&token)
                    .ok_or_else(|| anyhow::anyhow!("技能来源预览已过期，请重新扫描"))?;
                anyhow::ensure!(
                    preview.session == session && preview.workspace == workspace,
                    "技能来源不属于当前会话或工作区，请重新扫描"
                );
                anyhow::ensure!(!paths.is_empty(), "请选择要添加的技能");
                let unique = paths.iter().collect::<HashSet<_>>();
                anyhow::ensure!(unique.len() == paths.len(), "技能选择重复");
                let selected = paths
                    .iter()
                    .map(|path| {
                        let skill = preview
                            .skills
                            .iter()
                            .find(|skill| &skill.path == path)
                            .ok_or_else(|| anyhow::anyhow!("技能不在此来源预览中，请重新扫描"))?;
                        anyhow::ensure!(
                            !skill.conflict && !installed.contains_key(&skill.name),
                            "技能名称已存在，保留现有技能"
                        );
                        Ok(skill.clone())
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                for skill in selected {
                    installed.insert(skill.name.clone(), format!(
                        "---\nname: {}\ndescription: {}\n---\n\nImported from {}.\nSee references/guide.md and scripts/check.sh.\n",
                        skill.name, skill.description, skill.path,
                    ));
                }
                let mut names = installed.keys().collect::<Vec<_>>();
                names.sort();
                Ok(Snapshot::Skills(
                    names
                        .into_iter()
                        .map(|name| Skill {
                            name: name.clone(),
                            title: name.clone(),
                            description: "项目技能".into(),
                            source: "项目技能".into(),
                            editable: true,
                        })
                        .collect(),
                ))
            }
            _ => unreachable!("only source import actions reach this fixture"),
        }
    }
}
