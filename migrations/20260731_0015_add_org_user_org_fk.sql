ALTER TABLE `org_user` ADD CONSTRAINT `fk_org_user_org_org` FOREIGN KEY (`org_org`) REFERENCES `org_org` (`id`) ON UPDATE RESTRICT ON DELETE RESTRICT
