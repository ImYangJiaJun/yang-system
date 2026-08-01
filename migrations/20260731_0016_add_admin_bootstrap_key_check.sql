ALTER TABLE `admin_user` ADD CONSTRAINT `chk_admin_user_owner_key` CHECK (`owner_key` IS NULL OR `owner_key` = 'system-owner')
