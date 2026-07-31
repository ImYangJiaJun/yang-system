ALTER TABLE `users` ADD CONSTRAINT `chk_users_status` CHECK (`status` IN ('active', 'disabled'))
