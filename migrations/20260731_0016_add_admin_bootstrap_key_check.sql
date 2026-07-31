ALTER TABLE `admin_user` ADD CONSTRAINT `chk_admin_user_bootstrap_key` CHECK (`bootstrap_key` IS NULL OR `bootstrap_key` = 'initial-admin')
