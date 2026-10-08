CREATE TABLE project_plugins (
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                marketplace_source TEXT NOT NULL CHECK(length(marketplace_source) BETWEEN 1 AND 4096),
                plugin_name TEXT NOT NULL CHECK(length(plugin_name) BETWEEN 1 AND 256),
                package_source TEXT NOT NULL CHECK(length(package_source) BETWEEN 1 AND 4096),
                version TEXT CHECK(version IS NULL OR length(version) BETWEEN 1 AND 256),
                enabled_at INTEGER NOT NULL DEFAULT (unixepoch()),
                PRIMARY KEY(project_id, marketplace_source, plugin_name)
            );
            CREATE INDEX project_plugins_project_enabled ON project_plugins(project_id,enabled_at DESC,plugin_name);
            PRAGMA user_version=9;
