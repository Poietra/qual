from manim import *


class SuppressedLag(Scene):
    def construct(self):
        follower = Dot()
        driver = Dot()
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))  # manim-lint: ignore[MLR109]
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        self.add(follower, driver)
        self.wait(2)
