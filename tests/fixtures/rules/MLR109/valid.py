from manim import *


class OrderedScene(Scene):
    def construct(self):
        follower = Dot()
        driver = Dot()
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        # The writer runs first, so the follower sees this frame's state.
        self.add(driver, follower)
        self.wait(2)


class CompositeRead(Scene):
    def construct(self):
        follower = Dot()
        driver = Dot()
        # The expression is outside MLR109's direct dependency proof.
        follower.add_updater(lambda mob: mob.move_to(driver.get_center() + UP))
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        self.add(follower, driver)
        self.wait(2)


class GroupOrder(Scene):
    def construct(self):
        follower = Dot()
        driver = Dot()
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))
        driver.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        self.add(VGroup(follower, driver))
        self.wait(2)
