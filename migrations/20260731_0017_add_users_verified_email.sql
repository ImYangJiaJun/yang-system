ALTER TABLE `users`
    ADD COLUMN `email` VARCHAR(254) NULL AFTER `username`,
    ADD COLUMN `email_verified_at` BIGINT NULL AFTER `email`,
    ADD UNIQUE KEY `uk_users_email` (`email`),
    ADD CONSTRAINT `chk_users_verified_email_pair`
        CHECK ((`email` IS NULL AND `email_verified_at` IS NULL)
            OR (`email` IS NOT NULL AND `email_verified_at` IS NOT NULL));
