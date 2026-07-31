ALTER TABLE `admin_user` ADD CONSTRAINT `fk_admin_user_user_user` FOREIGN KEY (`user_user`) REFERENCES `users` (`id`) ON UPDATE RESTRICT ON DELETE RESTRICT
