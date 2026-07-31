ALTER TABLE `org_user` ADD CONSTRAINT `fk_org_user_user_user` FOREIGN KEY (`user_user`) REFERENCES `users` (`id`) ON UPDATE RESTRICT ON DELETE RESTRICT
