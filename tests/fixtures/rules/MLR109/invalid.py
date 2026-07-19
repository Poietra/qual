from manim import *


class LaggedScene(Scene):
    def construct(self):
        follower = Dot(color=BLUE)
        driver = Dot(color=RED)
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        self.add(follower, driver)
        self.wait(2)
